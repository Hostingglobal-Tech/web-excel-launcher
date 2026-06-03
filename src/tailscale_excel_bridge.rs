use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

const SAMPLE_WORKBOOK: &[u8] = include_bytes!("../assets/sample-web-excel-launcher.xlsx");

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
enum PromptAction {
    OpenExcel,
    OpenCsv,
    OpenGoogleSheets,
    OpenMicrosoftExcelWeb,
    OpenMicrosoftOfficeViewer,
    Deny,
}

fn main() -> std::io::Result<()> {
    let mut args = env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "help".to_string());
    match mode.as_str() {
        "agent" => run_agent(args.next().unwrap_or_else(default_agent_bind)),
        "gateway" => {
            let bind = args.next().unwrap_or_else(default_gateway_bind);
            let agent_url = args
                .next()
                .or_else(|| env::var("EXCEL_AGENT_URL").ok())
                .unwrap_or_else(|| "http://127.0.0.1:8788".to_string());
            run_gateway(bind, agent_url)
        }
        _ => {
            eprintln!("usage:");
            eprintln!("  tailscale-excel-bridge agent [bind]");
            eprintln!("  tailscale-excel-bridge gateway [bind] [agent_url]");
            Ok(())
        }
    }
}

fn run_agent(bind: String) -> std::io::Result<()> {
    require_bridge_token();
    let listener = TcpListener::bind(&bind)?;
    eprintln!("local excel agent listening on http://{bind}/");
    serve(listener, move |request| agent_route(request))
}

fn run_gateway(bind: String, agent_url: String) -> std::io::Result<()> {
    require_bridge_token();
    let listener = TcpListener::bind(&bind)?;
    eprintln!("vultr gateway listening on http://{bind}/ -> {agent_url}");
    serve(listener, move |request| gateway_route(request, &agent_url))
}

fn serve<F>(listener: TcpListener, handler: F) -> std::io::Result<()>
where
    F: Fn(HttpRequest) -> Response + Send + Sync + Clone + 'static,
{
    for stream in listener.incoming() {
        let handler = handler.clone();
        match stream {
            Ok(stream) => {
                thread::spawn(move || {
                    if let Err(err) = handle_client(stream, handler) {
                        eprintln!("request error: {err}");
                    }
                });
            }
            Err(err) => eprintln!("accept error: {err}"),
        }
    }
    Ok(())
}

fn handle_client<F>(mut stream: TcpStream, handler: F) -> std::io::Result<()>
where
    F: Fn(HttpRequest) -> Response,
{
    let Some(request) = read_request(&mut stream)? else {
        return Ok(());
    };
    let response = handler(request);
    write_response(
        &mut stream,
        response.status,
        response.content_type,
        &response.body,
    )
}

fn agent_route(request: HttpRequest) -> Response {
    let route = clean_route(&request.path);
    match (request.method.as_str(), route) {
        ("GET", "/health") => text("200 OK", "agent ok\n"),
        ("POST", "/run") | ("GET", "/run") => {
            if !authorized(&request) {
                return text("401 Unauthorized", "unauthorized\n");
            }
            let prompt = form_value(&request.body, "prompt")
                .or_else(|| query_value(&request.path, "prompt"))
                .unwrap_or_else(|| "엑셀 실행해줘".to_string());
            agent_execute_prompt(&prompt)
        }
        ("POST", "/run-computer") | ("GET", "/run-computer") => {
            if !authorized(&request) {
                return text("401 Unauthorized", "unauthorized\n");
            }
            let prompt = form_value(&request.body, "prompt")
                .or_else(|| query_value(&request.path, "prompt"))
                .unwrap_or_else(|| "엑셀 실행해줘".to_string());
            agent_execute_prompt_computer_use(&prompt)
        }
        ("GET", "/open-excel") => {
            if !authorized(&request) {
                return text("401 Unauthorized", "unauthorized\n");
            }
            match open_blank_excel() {
                Ok(msg) => text("200 OK", &format!("agent action=OPEN_EXCEL\n{msg}")),
                Err(err) => text("500 Internal Server Error", &err),
            }
        }
        ("POST", "/open-google-sheets") | ("GET", "/open-google-sheets") => {
            if !authorized(&request) {
                return text("401 Unauthorized", "unauthorized\n");
            }
            match open_google_sheets() {
                Ok(msg) => text("200 OK", &format!("agent action=OPEN_GOOGLE_SHEETS\n{msg}")),
                Err(err) => text("500 Internal Server Error", &err),
            }
        }
        ("POST", "/open-ms-excel-web") | ("GET", "/open-ms-excel-web") => {
            if !authorized(&request) {
                return text("401 Unauthorized", "unauthorized\n");
            }
            match open_microsoft_excel_web() {
                Ok(msg) => text(
                    "200 OK",
                    &format!("agent action=OPEN_MICROSOFT_EXCEL_WEB\n{msg}"),
                ),
                Err(err) => text("500 Internal Server Error", &err),
            }
        }
        ("POST", "/open-ms-office-viewer") | ("GET", "/open-ms-office-viewer") => {
            if !authorized(&request) {
                return text("401 Unauthorized", "unauthorized\n");
            }
            let src =
                form_value(&request.body, "src").or_else(|| query_value(&request.path, "src"));
            match open_microsoft_office_viewer(src.as_deref()) {
                Ok(msg) => text(
                    "200 OK",
                    &format!("agent action=OPEN_MICROSOFT_OFFICE_VIEWER\n{msg}"),
                ),
                Err(err) => text("500 Internal Server Error", &err),
            }
        }
        _ => text("404 Not Found", "not found\n"),
    }
}

fn gateway_route(request: HttpRequest, agent_url: &str) -> Response {
    let route = clean_route(&request.path);
    match (request.method.as_str(), route) {
        ("GET", "/") => html("200 OK", &gateway_index(agent_url)),
        ("GET", "/health") => text("200 OK", "gateway ok\n"),
        ("GET", "/sample.xlsx") => binary(
            "200 OK",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            SAMPLE_WORKBOOK.to_vec(),
        ),
        ("POST", "/prompt") => {
            let prompt = form_value(&request.body, "prompt").unwrap_or_default();
            let prompt = prompt.trim();
            if prompt.is_empty() {
                return html(
                    "400 Bad Request",
                    &gateway_result("프롬프트 오류", "프롬프트가 비어 있습니다.", agent_url, ""),
                );
            }
            match forward_to_agent(agent_url, prompt) {
                Ok(agent_reply) => html(
                    "200 OK",
                    &gateway_result(
                        "Vultr -> Tailscale -> 로컬 Excel",
                        &agent_reply,
                        agent_url,
                        prompt,
                    ),
                ),
                Err(err) => html(
                    "502 Bad Gateway",
                    &gateway_result("Agent 호출 실패", &err, agent_url, prompt),
                ),
            }
        }
        ("POST", "/prompt-computer") => {
            let prompt = form_value(&request.body, "prompt").unwrap_or_default();
            let prompt = prompt.trim();
            if prompt.is_empty() {
                return html(
                    "400 Bad Request",
                    &gateway_result("프롬프트 오류", "프롬프트가 비어 있습니다.", agent_url, ""),
                );
            }
            match forward_to_agent_endpoint(agent_url, "run-computer", prompt) {
                Ok(agent_reply) => html(
                    "200 OK",
                    &gateway_result(
                        "Vultr -> Tailscale -> OpenAI Computer Use -> 로컬 Excel",
                        &agent_reply,
                        agent_url,
                        prompt,
                    ),
                ),
                Err(err) => html(
                    "502 Bad Gateway",
                    &gateway_result("Computer Use Agent 호출 실패", &err, agent_url, prompt),
                ),
            }
        }
        ("POST", "/open-google-sheets") | ("GET", "/open-google-sheets") => {
            match forward_to_agent_endpoint(agent_url, "open-google-sheets", "") {
                Ok(agent_reply) => html(
                    "200 OK",
                    &gateway_result(
                        "Vultr -> Tailscale -> Google Sheets",
                        &agent_reply,
                        agent_url,
                        "",
                    ),
                ),
                Err(err) => html(
                    "502 Bad Gateway",
                    &gateway_result("Google Sheets Agent 호출 실패", &err, agent_url, ""),
                ),
            }
        }
        ("POST", "/open-ms-excel-web") | ("GET", "/open-ms-excel-web") => {
            match forward_to_agent_endpoint(agent_url, "open-ms-excel-web", "") {
                Ok(agent_reply) => html(
                    "200 OK",
                    &gateway_result(
                        "Vultr -> Tailscale -> Microsoft Excel for the web",
                        &agent_reply,
                        agent_url,
                        "",
                    ),
                ),
                Err(err) => html(
                    "502 Bad Gateway",
                    &gateway_result("Microsoft Excel Web Agent 호출 실패", &err, agent_url, ""),
                ),
            }
        }
        ("POST", "/open-ms-office-viewer") | ("GET", "/open-ms-office-viewer") => {
            let src = form_value(&request.body, "src")
                .or_else(|| query_value(&request.path, "src"))
                .unwrap_or_else(|| gateway_sample_workbook_url(&request));
            match forward_to_agent_endpoint_with_fields(
                agent_url,
                "open-ms-office-viewer",
                &[("src", src.as_str())],
            ) {
                Ok(agent_reply) => html(
                    "200 OK",
                    &gateway_result(
                        "Vultr -> Tailscale -> Microsoft Office Web Viewer",
                        &agent_reply,
                        agent_url,
                        "",
                    ),
                ),
                Err(err) => html(
                    "502 Bad Gateway",
                    &gateway_result(
                        "Microsoft Office Web Viewer Agent 호출 실패",
                        &err,
                        agent_url,
                        "",
                    ),
                ),
            }
        }
        _ => text("404 Not Found", "not found\n"),
    }
}

fn agent_execute_prompt(prompt: &str) -> Response {
    match classify_prompt_with_openai(prompt) {
        Ok(PromptAction::OpenExcel) => match open_blank_excel() {
            Ok(msg) => text(
                "200 OK",
                &format!("agent action=OPEN_EXCEL\nprompt={prompt}\n{msg}"),
            ),
            Err(err) => text("500 Internal Server Error", &err),
        },
        Ok(PromptAction::OpenCsv) => match create_csv_and_open() {
            Ok(msg) => text(
                "200 OK",
                &format!("agent action=OPEN_CSV\nprompt={prompt}\n{msg}"),
            ),
            Err(err) => text("500 Internal Server Error", &err),
        },
        Ok(PromptAction::OpenGoogleSheets) => match open_google_sheets() {
            Ok(msg) => text(
                "200 OK",
                &format!("agent action=OPEN_GOOGLE_SHEETS\nprompt={prompt}\n{msg}"),
            ),
            Err(err) => text("500 Internal Server Error", &err),
        },
        Ok(PromptAction::OpenMicrosoftExcelWeb) => match open_microsoft_excel_web() {
            Ok(msg) => text(
                "200 OK",
                &format!("agent action=OPEN_MICROSOFT_EXCEL_WEB\nprompt={prompt}\n{msg}"),
            ),
            Err(err) => text("500 Internal Server Error", &err),
        },
        Ok(PromptAction::OpenMicrosoftOfficeViewer) => match open_microsoft_office_viewer(None) {
            Ok(msg) => text(
                "200 OK",
                &format!("agent action=OPEN_MICROSOFT_OFFICE_VIEWER\nprompt={prompt}\n{msg}"),
            ),
            Err(err) => text("500 Internal Server Error", &err),
        },
        Ok(PromptAction::Deny) => text(
            "200 OK",
            &format!(
                "agent action=DENY\nprompt={prompt}\nExcel 실행 요청으로 판단하지 않았습니다.\n"
            ),
        ),
        Err(err) => text(
            "500 Internal Server Error",
            &format!("OpenAI API 실패: {err}\n"),
        ),
    }
}

fn agent_execute_prompt_computer_use(prompt: &str) -> Response {
    match run_computer_use_excel(prompt) {
        Ok(log) => text("200 OK", &log),
        Err(err) => text(
            "500 Internal Server Error",
            &format!("computer use 실패: {err}\n"),
        ),
    }
}

fn run_computer_use_excel(prompt: &str) -> Result<String, String> {
    let api_key = env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY 환경변수가 로컬 agent에 없습니다.".to_string())?;
    let mut log = String::new();
    log.push_str("mode=OpenAI computer tool\n");
    log.push_str("model=gpt-5.5\n");
    log.push_str("store=true (computer loop previous_response_id 필요)\n");

    let task = format!(
        "Windows desktop에서 Microsoft Excel을 실행해줘. \
         시작 메뉴나 실행 창을 사용해도 된다. Excel 창이 열리면 더 이상 조작하지 말고 멈춰라. \
         사용자 원문: {prompt}"
    );
    let mut body = call_openai_computer_initial(&api_key, &task)?;

    for step in 1..=8 {
        let Some(call_id) = jq_first_string(
            &body,
            ".output[]? | select(.type==\"computer_call\") | .call_id",
        )?
        else {
            log.push_str(&format!("step={step} computer_call 없음, 종료\n"));
            return Ok(log);
        };
        let action_lines = jq_lines(
            &body,
            r#".output[]? | select(.type=="computer_call") | .actions[]? |
if .type=="keypress" then "keypress\t" + ((.keys // []) | join("+"))
elif .type=="type" then "type\t" + (.text // "")
elif .type=="click" then "click\t" + ((.x // 0)|tostring) + "\t" + ((.y // 0)|tostring) + "\t" + (.button // "left")
elif .type=="double_click" then "double_click\t" + ((.x // 0)|tostring) + "\t" + ((.y // 0)|tostring)
elif .type=="move" then "move\t" + ((.x // 0)|tostring) + "\t" + ((.y // 0)|tostring)
elif .type=="wait" then "wait"
elif .type=="screenshot" then "screenshot"
else "unsupported\t" + .type
end"#,
        )?;

        if action_lines.is_empty() {
            log.push_str(&format!("step={step} action 없음, 종료\n"));
            return Ok(log);
        }

        log.push_str(&format!("step={step} call_id={call_id}\n"));
        for line in &action_lines {
            log.push_str("action=");
            log.push_str(line);
            log.push('\n');
        }
        execute_computer_action_lines(&action_lines)?;
        thread::sleep(std::time::Duration::from_millis(1000));

        if excel_process_seen() {
            log.push_str("verify=EXCEL.EXE 감지됨, computer loop 종료\n");
            return Ok(log);
        }

        let screenshot_b64 = capture_windows_screenshot_base64()?;
        let response_id = jq_first_string(&body, ".id")?
            .ok_or_else(|| "OpenAI response id를 찾지 못했습니다.".to_string())?;
        body = call_openai_computer_screenshot(&api_key, &response_id, &call_id, &screenshot_b64)?;

        if excel_process_seen() {
            log.push_str("verify=EXCEL.EXE 감지됨\n");
        }
    }

    Ok(log)
}

fn call_openai_computer_initial(api_key: &str, task: &str) -> Result<String, String> {
    let payload = format!(
        "{{\"model\":\"gpt-5.5\",\"tools\":[{{\"type\":\"computer\"}}],\"input\":{},\"max_output_tokens\":512,\"store\":true}}",
        json_string(task)
    );
    call_openai_json(api_key, &payload)
}

fn call_openai_computer_screenshot(
    api_key: &str,
    previous_response_id: &str,
    call_id: &str,
    screenshot_b64: &str,
) -> Result<String, String> {
    let image_url = format!("data:image/png;base64,{screenshot_b64}");
    let payload = format!(
        "{{\"model\":\"gpt-5.5\",\"tools\":[{{\"type\":\"computer\"}}],\"previous_response_id\":{},\"input\":[{{\"type\":\"computer_call_output\",\"call_id\":{},\"output\":{{\"type\":\"computer_screenshot\",\"image_url\":{},\"detail\":\"original\"}}}}],\"max_output_tokens\":512,\"store\":true}}",
        json_string(previous_response_id),
        json_string(call_id),
        json_string(&image_url)
    );
    call_openai_json(api_key, &payload)
}

fn call_openai_json(api_key: &str, payload: &str) -> Result<String, String> {
    let auth = format!("Authorization: Bearer {api_key}");
    let payload_path = write_secret_temp("openai-payload", ".json", payload)?;
    let data_ref = format!("@{}", payload_path.display());
    let config = format!(
        "silent\nshow-error\nmax-time = \"35\"\nurl = \"https://api.openai.com/v1/responses\"\nheader = {}\nheader = \"Content-Type: application/json\"\ndata-binary = {}\nwrite-out = \"\\n__HTTP_STATUS__:%{{http_code}}\"\n",
        curl_config_string(&auth),
        curl_config_string(&data_ref)
    );
    let config_path = write_secret_temp("openai-curl", ".conf", &config)?;
    let output = Command::new("curl")
        .args(["--config", config_path.to_string_lossy().as_ref()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| format!("curl 실행 실패: {err}"));
    let _ = fs::remove_file(&config_path);
    let _ = fs::remove_file(&payload_path);
    let output = output?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(format!(
            "OpenAI 호출 실패: status={} stderr={}",
            output.status,
            truncate(&stderr, 800)
        ));
    }
    let Some((body, status_text)) = stdout.rsplit_once("\n__HTTP_STATUS__:") else {
        return Err(format!(
            "OpenAI HTTP 상태를 읽지 못했습니다: {}",
            truncate(&stdout, 800)
        ));
    };
    let status = status_text.trim().parse::<u16>().unwrap_or(0);
    if !(200..300).contains(&status) {
        return Err(format!("OpenAI HTTP {status}: {}", truncate(body, 1200)));
    }
    Ok(body.to_string())
}

fn jq_first_string(input: &str, filter: &str) -> Result<Option<String>, String> {
    let lines = jq_lines(input, filter)?;
    Ok(lines
        .into_iter()
        .find(|line| !line.trim().is_empty() && line.trim() != "null"))
}

fn jq_lines(input: &str, filter: &str) -> Result<Vec<String>, String> {
    let mut child = Command::new("jq")
        .args(["-r", filter])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("jq 실행 실패: {err}"))?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(input.as_bytes())
            .map_err(|err| format!("jq stdin 쓰기 실패: {err}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("jq 대기 실패: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "jq 실패: {}",
            truncate(&String::from_utf8_lossy(&output.stderr), 800)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.to_string())
        .collect())
}

fn execute_computer_action_lines(lines: &[String]) -> Result<(), String> {
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        match parts.next().unwrap_or("") {
            "screenshot" => {}
            "wait" => thread::sleep(std::time::Duration::from_millis(800)),
            "keypress" => {
                let keys = parts.next().unwrap_or("");
                press_keys(keys)?;
                thread::sleep(std::time::Duration::from_millis(400));
            }
            "type" => {
                let text = parts.next().unwrap_or("");
                type_text(text)?;
                thread::sleep(std::time::Duration::from_millis(400));
            }
            "click" => {
                let x = parts.next().unwrap_or("0").parse::<i32>().unwrap_or(0);
                let y = parts.next().unwrap_or("0").parse::<i32>().unwrap_or(0);
                click_mouse(x, y, false)?;
                thread::sleep(std::time::Duration::from_millis(400));
            }
            "double_click" => {
                let x = parts.next().unwrap_or("0").parse::<i32>().unwrap_or(0);
                let y = parts.next().unwrap_or("0").parse::<i32>().unwrap_or(0);
                click_mouse(x, y, true)?;
                thread::sleep(std::time::Duration::from_millis(400));
            }
            "move" => {
                let x = parts.next().unwrap_or("0").parse::<i32>().unwrap_or(0);
                let y = parts.next().unwrap_or("0").parse::<i32>().unwrap_or(0);
                move_mouse(x, y)?;
            }
            other => return Err(format!("지원하지 않는 computer action: {other}")),
        }
    }
    Ok(())
}

fn capture_windows_screenshot_base64() -> Result<String, String> {
    let path = demo_screenshot_path();
    let win_path =
        wsl_to_windows_path(&path).ok_or_else(|| format!("Windows 경로 변환 실패: {path:?}"))?;
    let ps = format!(
        "Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; \
         $b=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds; \
         $bmp=New-Object System.Drawing.Bitmap $b.Width,$b.Height; \
         $g=[System.Drawing.Graphics]::FromImage($bmp); \
         $g.CopyFromScreen($b.Location,[System.Drawing.Point]::Empty,$b.Size); \
         $bmp.Save({},[System.Drawing.Imaging.ImageFormat]::Png); \
         $g.Dispose(); $bmp.Dispose()",
        powershell_single_quote(&win_path)
    );
    run_powershell_wait(&ps)?;
    let output = Command::new("base64")
        .args(["-w0", path.to_string_lossy().as_ref()])
        .output()
        .map_err(|err| format!("base64 실행 실패: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "base64 실패: {}",
            truncate(&String::from_utf8_lossy(&output.stderr), 800)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn press_keys(keys: &str) -> Result<(), String> {
    let codes: Vec<u8> = keys
        .split('+')
        .filter_map(|key| key_to_vk(key.trim()))
        .collect();
    if codes.is_empty() {
        return Err(format!("keypress 키를 해석하지 못했습니다: {keys}"));
    }
    let down = codes
        .iter()
        .map(|code| format!("[K]::keybd_event({code},0,0,0);"))
        .collect::<Vec<_>>()
        .join("");
    let up = codes
        .iter()
        .rev()
        .map(|code| format!("[K]::keybd_event({code},0,2,0);"))
        .collect::<Vec<_>>()
        .join("");
    let ps = format!(
        "Add-Type @'\nusing System; using System.Runtime.InteropServices; public class K {{ [DllImport(\"user32.dll\")] public static extern void keybd_event(byte bVk, byte bScan, int dwFlags, int dwExtraInfo); }}\n'@; {down} Start-Sleep -Milliseconds 80; {up}"
    );
    run_powershell_wait(&ps)
}

fn key_to_vk(key: &str) -> Option<u8> {
    let upper = key.to_ascii_uppercase();
    match upper.as_str() {
        "WIN" | "WINDOWS" | "META" | "CMD" | "COMMAND" => Some(0x5B),
        "ENTER" | "RETURN" => Some(0x0D),
        "ESC" | "ESCAPE" => Some(0x1B),
        "TAB" => Some(0x09),
        "SPACE" => Some(0x20),
        "BACKSPACE" => Some(0x08),
        "DELETE" | "DEL" => Some(0x2E),
        "CTRL" | "CONTROL" => Some(0x11),
        "SHIFT" => Some(0x10),
        "ALT" | "OPTION" => Some(0x12),
        "UP" | "ARROWUP" => Some(0x26),
        "DOWN" | "ARROWDOWN" => Some(0x28),
        "LEFT" | "ARROWLEFT" => Some(0x25),
        "RIGHT" | "ARROWRIGHT" => Some(0x27),
        _ if upper.len() == 1 => upper.as_bytes().first().copied(),
        _ => None,
    }
}

fn type_text(text: &str) -> Result<(), String> {
    let safe: String = text
        .chars()
        .filter(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '.' | '-' | '_' | ':' | '\\' | '/')
        })
        .collect();
    if safe.is_empty() {
        return Err("type action 텍스트가 비어 있거나 허용 문자가 아닙니다.".to_string());
    }
    let ps = format!(
        "Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.SendKeys]::SendWait({})",
        powershell_single_quote(&safe)
    );
    run_powershell_wait(&ps)
}

fn move_mouse(x: i32, y: i32) -> Result<(), String> {
    let ps = format!(
        "Add-Type @'\nusing System; using System.Runtime.InteropServices; public class M {{ [DllImport(\"user32.dll\")] public static extern bool SetCursorPos(int X, int Y); }}\n'@; [M]::SetCursorPos({x},{y}) | Out-Null"
    );
    run_powershell_wait(&ps)
}

fn click_mouse(x: i32, y: i32, double_click: bool) -> Result<(), String> {
    let repeat = if double_click { 2 } else { 1 };
    let ps = format!(
        "Add-Type @'\nusing System; using System.Runtime.InteropServices; public class M {{ [DllImport(\"user32.dll\")] public static extern bool SetCursorPos(int X, int Y); [DllImport(\"user32.dll\")] public static extern void mouse_event(int dwFlags, int dx, int dy, int dwData, int dwExtraInfo); }}\n'@; [M]::SetCursorPos({x},{y}) | Out-Null; for ($i=0; $i -lt {repeat}; $i++) {{ [M]::mouse_event(2,0,0,0,0); Start-Sleep -Milliseconds 60; [M]::mouse_event(4,0,0,0,0); Start-Sleep -Milliseconds 120; }}"
    );
    run_powershell_wait(&ps)
}

fn excel_process_seen() -> bool {
    Command::new(powershell_path())
        .args([
            "-NoProfile",
            "-Command",
            "if (Get-Process -Name EXCEL -ErrorAction SilentlyContinue) { exit 0 } else { exit 1 }",
        ])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn run_powershell_wait(command: &str) -> Result<(), String> {
    let output = Command::new(powershell_path())
        .args([
            "-NoProfile",
            "-STA",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            command,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| format!("powershell.exe 실행 실패: {err}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "powershell 실패: {}",
            truncate(&String::from_utf8_lossy(&output.stderr), 800)
        ))
    }
}

fn powershell_path() -> &'static str {
    const CANDIDATES: [&str; 3] = [
        "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe",
        "/mnt/c/Windows/System32/powershell.exe",
        "powershell.exe",
    ];
    for candidate in CANDIDATES {
        if candidate.contains('/') && Path::new(candidate).exists() {
            return candidate;
        }
    }
    "powershell.exe"
}

fn forward_to_agent(agent_url: &str, prompt: &str) -> Result<String, String> {
    forward_to_agent_endpoint(agent_url, "run", prompt)
}

fn forward_to_agent_endpoint(
    agent_url: &str,
    endpoint: &str,
    prompt: &str,
) -> Result<String, String> {
    forward_to_agent_endpoint_with_fields(agent_url, endpoint, &[("prompt", prompt)])
}

fn forward_to_agent_endpoint_with_fields(
    agent_url: &str,
    endpoint: &str,
    fields: &[(&str, &str)],
) -> Result<String, String> {
    let token = bridge_token();
    let run_url = format!("{}/{}", agent_url.trim_end_matches('/'), endpoint);
    let header = format!("X-Excel-Bridge-Token: {token}");
    let form = fields
        .iter()
        .map(|(key, value)| format!("{}={}", url_encode(key), url_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    let timeout = if endpoint == "run-computer" {
        "100"
    } else {
        "30"
    };
    let form_path = write_secret_temp("excel-agent-form", ".txt", &form)?;
    let data_ref = format!("@{}", form_path.display());
    let config = format!(
        "silent\nshow-error\nmax-time = {}\nurl = {}\nheader = {}\nheader = \"Content-Type: application/x-www-form-urlencoded\"\ndata-binary = {}\n",
        curl_config_string(timeout),
        curl_config_string(&run_url),
        curl_config_string(&header),
        curl_config_string(&data_ref)
    );
    let config_path = write_secret_temp("excel-agent-curl", ".conf", &config)?;
    let output = Command::new("curl")
        .args(["--config", config_path.to_string_lossy().as_ref()])
        .output()
        .map_err(|err| format!("curl 실행 실패: {err}"));
    let _ = fs::remove_file(&config_path);
    let _ = fs::remove_file(&form_path);
    let output = output?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(format!(
            "agent HTTP 호출 실패: status={} stderr={} stdout={}",
            output.status,
            truncate(&stderr, 800),
            truncate(&stdout, 800)
        ))
    }
}

fn gateway_sample_workbook_url(request: &HttpRequest) -> String {
    env::var("EXCEL_GATEWAY_PUBLIC_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let host = request
                .headers
                .get("host")
                .cloned()
                .unwrap_or_else(|| "127.0.0.1:8878".to_string());
            format!("http://{host}")
        })
        + "/sample.xlsx"
}

fn classify_prompt_with_openai(prompt: &str) -> Result<PromptAction, String> {
    let api_key = env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY 환경변수가 로컬 agent에 없습니다.".to_string())?;
    let model = env::var("OPENAI_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "gpt-5.5".to_string());
    let instructions = "\
한국어 명령 분류기. 한 단어만 출력: OPEN_EXCEL, OPEN_CSV, OPEN_GOOGLE_SHEETS, OPEN_MICROSOFT_EXCEL_WEB, OPEN_MICROSOFT_OFFICE_VIEWER, DENY. \
엑셀 실행/열기/켜줘는 OPEN_EXCEL. CSV/표 데이터/샘플 CSV는 OPEN_CSV. \
구글 스프레드시트/구글시트/Google Sheets는 OPEN_GOOGLE_SHEETS. \
마이크로소프트 웹용 엑셀/Excel for the web/Microsoft 365 Excel/웹 엑셀은 OPEN_MICROSOFT_EXCEL_WEB. \
MS Office Web Viewer/Office Web Viewer/엑셀 웹 뷰어/웹용 엑셀 뷰어/뷰어로 보기 요청은 OPEN_MICROSOFT_OFFICE_VIEWER. 나머지는 DENY.";
    let input = format!("요청: {prompt}");
    let payload = format!(
        "{{\"model\":{},\"instructions\":{},\"input\":{},\"reasoning\":{{\"effort\":\"none\"}},\"max_output_tokens\":64,\"store\":false}}",
        json_string(&model),
        json_string(instructions),
        json_string(&input)
    );
    let body = call_openai_responses_api(&api_key, &payload)?;
    let decision_area = openai_output_area(&body);
    if decision_area.contains("OPEN_MICROSOFT_OFFICE_VIEWER") {
        Ok(PromptAction::OpenMicrosoftOfficeViewer)
    } else if decision_area.contains("OPEN_MICROSOFT_EXCEL_WEB") {
        Ok(PromptAction::OpenMicrosoftExcelWeb)
    } else if decision_area.contains("OPEN_GOOGLE_SHEETS") {
        Ok(PromptAction::OpenGoogleSheets)
    } else if decision_area.contains("OPEN_CSV") {
        Ok(PromptAction::OpenCsv)
    } else if decision_area.contains("OPEN_EXCEL") {
        Ok(PromptAction::OpenExcel)
    } else if decision_area.contains("DENY") {
        Ok(PromptAction::Deny)
    } else {
        Err(format!(
            "OpenAI 응답에서 실행 토큰을 찾지 못했습니다: {}",
            truncate(&body, 800)
        ))
    }
}

fn call_openai_responses_api(api_key: &str, payload: &str) -> Result<String, String> {
    let auth = format!("Authorization: Bearer {api_key}");
    let payload_path = write_secret_temp("openai-payload", ".json", payload)?;
    let data_ref = format!("@{}", payload_path.display());
    let config = format!(
        "silent\nshow-error\nmax-time = \"25\"\nurl = \"https://api.openai.com/v1/responses\"\nheader = {}\nheader = \"Content-Type: application/json\"\ndata-binary = {}\nwrite-out = \"\\n__HTTP_STATUS__:%{{http_code}}\"\n",
        curl_config_string(&auth),
        curl_config_string(&data_ref)
    );
    let config_path = write_secret_temp("openai-curl", ".conf", &config)?;
    let output = Command::new("curl")
        .args(["--config", config_path.to_string_lossy().as_ref()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| format!("curl 실행 실패: {err}"));
    let _ = fs::remove_file(&config_path);
    let _ = fs::remove_file(&payload_path);
    let output = output?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(format!(
            "OpenAI API 호출 실패: status={} stderr={}",
            output.status,
            truncate(&stderr, 800)
        ));
    }

    let Some((body, status_text)) = stdout.rsplit_once("\n__HTTP_STATUS__:") else {
        return Err(format!(
            "OpenAI HTTP 상태를 읽지 못했습니다: {}",
            truncate(&stdout, 800)
        ));
    };
    let status = status_text.trim().parse::<u16>().unwrap_or(0);
    if !(200..300).contains(&status) {
        return Err(format!("OpenAI HTTP {status}: {}", truncate(body, 1200)));
    }
    Ok(body.to_string())
}

fn openai_output_area(body: &str) -> &str {
    if let Some(pos) = body.rfind("\"output_text\"") {
        &body[pos..]
    } else if let Some(pos) = body.rfind("\"output\"") {
        &body[pos..]
    } else {
        body
    }
}

fn open_blank_excel() -> Result<String, String> {
    spawn_powershell("Start-Process -FilePath 'excel.exe'")?;
    Ok("로컬 Windows Excel 실행 요청을 보냈습니다.\n".to_string())
}

fn create_csv_and_open() -> Result<String, String> {
    let csv_path = demo_csv_path();
    if let Some(parent) = csv_path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("폴더 생성 실패 {parent:?}: {err}"))?;
    }

    let csv = "품목,수량,단가,합계\n라우터,2,120000,240000\n스위치,3,80000,240000\n";
    fs::write(&csv_path, csv).map_err(|err| format!("CSV 쓰기 실패 {csv_path:?}: {err}"))?;
    let win_path = wsl_to_windows_path(&csv_path)
        .ok_or_else(|| format!("Windows 경로 변환 실패: {csv_path:?}"))?;
    let command = format!(
        "Start-Process -FilePath 'excel.exe' -ArgumentList {}",
        powershell_single_quote(&win_path)
    );
    spawn_powershell(&command)?;
    Ok(format!(
        "CSV를 만들고 로컬 Excel 실행 요청을 보냈습니다.\nWSL: {csv_path:?}\nWindows: {win_path}\n"
    ))
}

fn open_google_sheets() -> Result<String, String> {
    let url = google_sheets_url();
    let command = format!("Start-Process {}", powershell_single_quote(&url));
    spawn_powershell(&command)?;
    Ok(format!(
        "Google Sheets 새 스프레드시트 실행 요청을 보냈습니다.\nURL: {url}\n기존 브라우저의 Google 로그인 세션을 사용합니다.\n"
    ))
}

fn google_sheets_url() -> String {
    env::var("GOOGLE_SHEETS_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://docs.google.com/spreadsheets/create".to_string())
}

fn open_microsoft_excel_web() -> Result<String, String> {
    let url = microsoft_excel_web_url();
    open_browser_url(&url)?;
    Ok(format!(
        "Microsoft Excel for the web 실행 요청을 보냈습니다.\nURL: {url}\n브라우저의 기존 Microsoft 365 로그인 세션을 사용합니다.\n"
    ))
}

fn open_microsoft_office_viewer(explicit_src: Option<&str>) -> Result<String, String> {
    let src = office_viewer_source_url(explicit_src)?;
    let url = microsoft_office_viewer_url(&src);
    open_browser_url(&url)?;
    Ok(format!(
        "Microsoft Office Web Viewer 실행 요청을 보냈습니다.\nWorkbook: {src}\nViewer: {url}\n"
    ))
}

fn open_browser_url(url: &str) -> Result<(), String> {
    let command = format!("Start-Process {}", powershell_single_quote(url));
    spawn_powershell(&command)
}

fn microsoft_excel_web_url() -> String {
    env::var("MS_EXCEL_WEB_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://www.microsoft365.com/launch/excel?auth=1".to_string())
}

fn office_viewer_source_url(explicit_src: Option<&str>) -> Result<String, String> {
    if let Some(value) = explicit_src
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(value.to_string());
    }
    env::var("MS_OFFICE_VIEWER_SRC_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "MS_OFFICE_VIEWER_SRC_URL 또는 src 파라미터가 필요합니다. Office Web Viewer는 Microsoft 서버가 접근 가능한 공개 .xlsx URL만 볼 수 있습니다.".to_string()
        })
}

fn microsoft_office_viewer_url(src: &str) -> String {
    format!(
        "https://view.officeapps.live.com/op/view.aspx?src={}",
        url_encode(src)
    )
}

fn spawn_powershell(command: &str) -> Result<(), String> {
    Command::new(powershell_path())
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            command,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("powershell.exe 실행 실패: {err}"))
}

fn gateway_index(agent_url: &str) -> String {
    let prompt_path = gateway_path("/prompt");
    let prompt_computer_path = gateway_path("/prompt-computer");
    let google_sheets_path = gateway_path("/open-google-sheets");
    let ms_excel_web_path = gateway_path("/open-ms-excel-web");
    let ms_office_viewer_path = gateway_path("/open-ms-office-viewer");
    format!(
        r#"<!doctype html>
<html lang="ko">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Vultr Tailscale Spreadsheet Gateway</title>
  <style>
    body {{ margin: 0; font-family: system-ui, -apple-system, "Segoe UI", "Malgun Gothic", sans-serif; color: #111827; background: #f5f7fb; }}
    main {{ width: min(880px, calc(100vw - 32px)); margin: 48px auto; background: #fff; border: 1px solid #d7dde8; border-radius: 8px; padding: 28px; box-shadow: 0 16px 36px rgba(31, 41, 55, .08); }}
    h1 {{ margin: 0 0 8px; font-size: 28px; letter-spacing: 0; }}
    p, label {{ line-height: 1.55; color: #4b5563; }}
    label {{ display: block; margin: 22px 0 8px; font-weight: 700; color: #111827; }}
    input {{ box-sizing: border-box; width: 100%; min-height: 44px; padding: 0 12px; border: 1px solid #b8c1d1; border-radius: 6px; font-size: 16px; }}
    button {{ min-height: 42px; padding: 0 16px; border: 1px solid #1f4f8f; border-radius: 6px; background: #2563eb; color: #fff; font-weight: 650; cursor: pointer; font-size: 15px; }}
    code {{ background: #eef2f7; padding: 2px 5px; border-radius: 4px; }}
  </style>
</head>
<body>
  <main>
    <h1>Vultr에서 Tailscale로 Excel / Google Sheets 실행</h1>
    <p>
      이 화면은 Vultr 노드의 gateway가 제공합니다. 버튼을 누르면 Vultr가 Tailscale 주소
      <code>{agent_url}</code>의 WSL local agent에 요청하고, local agent가 allowlist된 Excel 또는 Google Sheets 액션을 실행합니다.
    </p>
    <form method="post" action="{prompt_path}">
      <label for="prompt">한글 프롬프트</label>
      <input id="prompt" name="prompt" value="엑셀 실행해줘" autocomplete="off">
      <p><button type="submit">Vultr에서 로컬 PC Excel 실행</button></p>
    </form>
    <form method="post" action="{prompt_computer_path}">
      <input name="prompt" value="엑셀 실행해줘" type="hidden">
      <p><button type="submit">OpenAI Computer Use로 Excel 실행</button></p>
    </form>
    <form method="post" action="{google_sheets_path}">
      <p><button type="submit">Google Sheets 새 스프레드시트 열기</button></p>
    </form>
    <form method="post" action="{ms_excel_web_path}">
      <p><button type="submit">Microsoft Excel for the web 열기</button></p>
    </form>
    <form method="post" action="{ms_office_viewer_path}">
      <p><button type="submit">Microsoft Office Web Viewer로 샘플 .xlsx 보기</button></p>
    </form>
  </main>
</body>
</html>
"#,
        agent_url = html_escape(agent_url),
        prompt_path = html_escape(&prompt_path),
        prompt_computer_path = html_escape(&prompt_computer_path),
        google_sheets_path = html_escape(&google_sheets_path),
        ms_excel_web_path = html_escape(&ms_excel_web_path),
        ms_office_viewer_path = html_escape(&ms_office_viewer_path)
    )
}

fn gateway_result(title: &str, message: &str, agent_url: &str, prompt: &str) -> String {
    let home_path = gateway_path("/");
    let prompt_line = if prompt.is_empty() {
        String::new()
    } else {
        format!(
            "<p><strong>프롬프트:</strong> <code>{}</code></p>",
            html_escape(prompt)
        )
    };
    format!(
        r#"<!doctype html>
<html lang="ko">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>실행 결과</title>
  <style>
    body {{ margin: 0; font-family: system-ui, -apple-system, "Segoe UI", "Malgun Gothic", sans-serif; color: #111827; background: #f5f7fb; }}
    main {{ width: min(820px, calc(100vw - 32px)); margin: 48px auto; background: #fff; border: 1px solid #d7dde8; border-radius: 8px; padding: 28px; box-shadow: 0 16px 36px rgba(31, 41, 55, .08); }}
    h1 {{ margin: 0 0 10px; font-size: 26px; letter-spacing: 0; }}
    p {{ line-height: 1.55; color: #4b5563; }}
    code {{ background: #eef2f7; padding: 2px 5px; border-radius: 4px; }}
    pre {{ white-space: pre-wrap; overflow-wrap: anywhere; background: #111827; color: #f9fafb; padding: 14px; border-radius: 6px; }}
    a {{ display: inline-flex; align-items: center; justify-content: center; min-height: 40px; padding: 0 14px; border-radius: 6px; background: #2563eb; color: white; text-decoration: none; font-weight: 650; }}
  </style>
</head>
<body>
  <main>
    <h1>{title}</h1>
    <p><strong>경로:</strong> Vultr gateway -> Tailscale -> WSL local agent -> allowlist action</p>
    <p><strong>Agent:</strong> <code>{agent_url}</code></p>
    {prompt_line}
    <pre>{message}</pre>
    <a href="{home_path}">돌아가기</a>
  </main>
</body>
</html>
"#,
        title = html_escape(title),
        agent_url = html_escape(agent_url),
        home_path = html_escape(&home_path),
        prompt_line = prompt_line,
        message = html_escape(message)
    )
}

#[derive(Debug)]
struct Response {
    status: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

fn text(status: &'static str, body: &str) -> Response {
    Response {
        status,
        content_type: "text/plain; charset=utf-8",
        body: body.as_bytes().to_vec(),
    }
}

fn html(status: &'static str, body: &str) -> Response {
    Response {
        status,
        content_type: "text/html; charset=utf-8",
        body: body.as_bytes().to_vec(),
    }
}

fn binary(status: &'static str, content_type: &'static str, body: Vec<u8>) -> Response {
    Response {
        status,
        content_type,
        body,
    }
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<HttpRequest>> {
    let mut data = Vec::new();
    let mut buffer = [0_u8; 8192];
    let bytes_read = stream.read(&mut buffer)?;
    if bytes_read == 0 {
        return Ok(None);
    }
    data.extend_from_slice(&buffer[..bytes_read]);

    let header_end = loop {
        if let Some(pos) = find_bytes(&data, b"\r\n\r\n") {
            break pos;
        }
        if data.len() > 64 * 1024 {
            break data.len();
        }
        let n = stream.read(&mut buffer)?;
        if n == 0 {
            break data.len();
        }
        data.extend_from_slice(&buffer[..n]);
    };

    let headers_text = String::from_utf8_lossy(&data[..header_end]);
    let mut lines = headers_text.lines();
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let mut headers = HashMap::new();
    let mut content_length = 0_usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let key = name.trim().to_ascii_lowercase();
            let val = value.trim().to_string();
            if key == "content-length" {
                content_length = val.parse::<usize>().unwrap_or(0);
            }
            headers.insert(key, val);
        }
    }

    let body_start = header_end.saturating_add(4);
    while data.len() < body_start.saturating_add(content_length) {
        let n = stream.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buffer[..n]);
    }

    let body_end = body_start.saturating_add(content_length).min(data.len());
    let body = if body_start <= body_end && body_start <= data.len() {
        data[body_start..body_end].to_vec()
    } else {
        Vec::new()
    };

    Ok(Some(HttpRequest {
        method,
        path,
        headers,
        body,
    }))
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

fn authorized(request: &HttpRequest) -> bool {
    let expected = bridge_token();
    if request
        .headers
        .get("x-excel-bridge-token")
        .map(|value| value == &expected)
        .unwrap_or(false)
    {
        return true;
    }
    query_value(&request.path, "token")
        .map(|value| value == expected)
        .unwrap_or(false)
}

fn require_bridge_token() {
    if env::var("EXCEL_BRIDGE_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_none()
    {
        eprintln!("EXCEL_BRIDGE_TOKEN is required");
        std::process::exit(2);
    }
}

fn bridge_token() -> String {
    env::var("EXCEL_BRIDGE_TOKEN").unwrap_or_default()
}

fn clean_route(path: &str) -> &str {
    let route = path.split('?').next().unwrap_or("/");
    let prefix = gateway_path_prefix();
    if prefix.is_empty() {
        return route;
    }
    if route == prefix {
        return "/";
    }
    if route.starts_with(&prefix) && route.as_bytes().get(prefix.len()) == Some(&b'/') {
        return &route[prefix.len()..];
    }
    route
}

fn gateway_path(path: &str) -> String {
    let prefix = gateway_path_prefix();
    if prefix.is_empty() {
        return path.to_string();
    }
    if path == "/" {
        prefix
    } else {
        format!("{prefix}{path}")
    }
}

fn gateway_path_prefix() -> String {
    env::var("EXCEL_GATEWAY_PATH_PREFIX")
        .ok()
        .map(|value| {
            let trimmed = value.trim().trim_end_matches('/').to_string();
            if trimmed == "/" {
                String::new()
            } else if trimmed.starts_with('/') {
                trimmed
            } else if trimmed.is_empty() {
                String::new()
            } else {
                format!("/{trimmed}")
            }
        })
        .unwrap_or_default()
}

fn default_agent_bind() -> String {
    env::var("EXCEL_AGENT_BIND").unwrap_or_else(|_| "127.0.0.1:8788".to_string())
}

fn default_gateway_bind() -> String {
    env::var("EXCEL_GATEWAY_BIND").unwrap_or_else(|_| "0.0.0.0:8878".to_string())
}

fn demo_csv_path() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let filename = format!("tailscale-excel-demo-{stamp}.csv");
    let desktop = Path::new("/mnt/c/Users/David/Desktop");
    if desktop.is_dir() {
        desktop.join(filename)
    } else {
        env::temp_dir().join(filename)
    }
}

fn demo_screenshot_path() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let filename = format!("tailscale-excel-cua-screen-{stamp}.png");
    let desktop = Path::new("/mnt/c/Users/David/Desktop");
    if desktop.is_dir() {
        desktop.join(filename)
    } else {
        env::temp_dir().join(filename)
    }
}

fn wsl_to_windows_path(path: &Path) -> Option<String> {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix("/mnt/c/") {
        return Some(format!("C:\\{}", rest.replace('/', "\\")));
    }
    if let Some(rest) = text.strip_prefix("/tmp/") {
        return Some(format!(
            "\\\\wsl.localhost\\Ubuntu\\tmp\\{}",
            rest.replace('/', "\\")
        ));
    }
    None
}

fn powershell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn form_value(body: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    query_param(&text, name)
}

fn query_value(path: &str, name: &str) -> Option<String> {
    path.split_once('?')
        .and_then(|(_, query)| query_param(query, name))
}

fn query_param(query: &str, name: &str) -> Option<String> {
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if url_decode(key) == name {
            return Some(url_decode(value));
        }
    }
    None
}

fn url_decode(value: &str) -> String {
    let mut out = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                if let (Some(high), Some(low)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2]))
                {
                    out.push((high << 4) | low);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn url_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push('+'),
            byte => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn curl_config_string(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("\"{escaped}\"")
}

fn write_secret_temp(prefix: &str, suffix: &str, content: &str) -> Result<PathBuf, String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);

    for attempt in 0..20 {
        let path = env::temp_dir().join(format!(
            "{prefix}-{}-{stamp}-{attempt}{suffix}",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(content.as_bytes())
                    .map_err(|err| format!("임시 파일 쓰기 실패 {path:?}: {err}"))?;
                return Ok(path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(format!("임시 파일 생성 실패 {path:?}: {err}")),
        }
    }

    Err("임시 파일 이름을 만들지 못했습니다.".to_string())
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch < ' ' => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn html_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            ch => out.push(ch),
        }
    }
    out
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (index, ch) in value.chars().enumerate() {
        if index >= max_chars {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out
}

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
enum PromptAction {
    OpenExcel,
    OpenCsv,
    OpenGoogleSheets,
    CreateGoogleSheets,
    OpenMicrosoftExcelWeb,
    OpenMicrosoftOfficeViewer,
    Deny,
}

fn main() -> std::io::Result<()> {
    let bind = env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:8877".to_string());
    let listener = TcpListener::bind(&bind)?;
    eprintln!("web-excel-launcher listening on http://{bind}/");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                thread::spawn(|| {
                    if let Err(err) = handle_client(stream) {
                        eprintln!("request error: {err}");
                    }
                });
            }
            Err(err) => eprintln!("accept error: {err}"),
        }
    }

    Ok(())
}

fn handle_client(mut stream: TcpStream) -> std::io::Result<()> {
    let Some(request) = read_request(&mut stream)? else {
        return Ok(());
    };

    let route = request.path.split('?').next().unwrap_or("/");
    let (status, content_type, body) = match (request.method.as_str(), route) {
        ("GET", "/") => ("200 OK", "text/html; charset=utf-8", index_html()),
        ("GET", "/health") => ("200 OK", "text/plain; charset=utf-8", "ok\n".to_string()),
        ("GET", "/open-excel") => action_response("수동 실행", "엑셀 실행", open_blank_excel()),
        ("GET", "/open-csv") => {
            action_response("수동 실행", "CSV 생성 후 엑셀 실행", create_csv_and_open())
        }
        ("GET", "/open-google-sheets") => {
            action_response("수동 실행", "Google Sheets 실행", open_google_sheets())
        }
        ("GET", "/create-gsheet") | ("POST", "/create-gsheet") => action_response(
            "수동 실행",
            "Google Sheets 생성 화면 실행",
            open_google_sheets(),
        ),
        ("GET", "/open-ms-excel-web") => action_response(
            "수동 실행",
            "Microsoft Excel for the web 실행",
            open_microsoft_excel_web(),
        ),
        ("GET", "/open-ms-office-viewer") => action_response(
            "수동 실행",
            "Microsoft Office Web Viewer 실행",
            open_microsoft_office_viewer(query_value(&request.path, "src").as_deref()),
        ),
        ("POST", "/prompt") => prompt_response(form_value(&request.body, "prompt")),
        ("GET", "/prompt") => prompt_response(query_value(&request.path, "prompt")),
        _ => (
            "404 Not Found",
            "text/plain; charset=utf-8",
            "not found\n".to_string(),
        ),
    };

    write_response(&mut stream, status, content_type, body.as_bytes())
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

    let headers = String::from_utf8_lossy(&data[..header_end]);
    let mut lines = headers.lines();
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let mut content_length = 0_usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse::<usize>().unwrap_or(0);
            }
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

    Ok(Some(HttpRequest { method, path, body }))
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    // Redirect convention: status "302 Found" carries the Location URL in `body`.
    // Lets the requester's own browser navigate to a real web spreadsheet
    // (no local Start-Process), so it also works over the remote/Tailscale path.
    if status.starts_with("302") {
        let location = String::from_utf8_lossy(body);
        write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?;
        return Ok(());
    }
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

fn index_html() -> String {
    let model = html_escape(&openai_model());
    format!(
        r#"<!doctype html>
<html lang="ko">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>한글 명령 Excel 실행기</title>
  <style>
    body {{
      margin: 0;
      font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", "Malgun Gothic", sans-serif;
      color: #111827;
      background: #f5f7fb;
    }}
    main {{
      width: min(840px, calc(100vw - 32px));
      margin: 48px auto;
      background: #ffffff;
      border: 1px solid #d7dde8;
      border-radius: 8px;
      padding: 28px;
      box-shadow: 0 16px 36px rgba(31, 41, 55, 0.08);
    }}
    h1 {{
      margin: 0 0 8px;
      font-size: 28px;
      letter-spacing: 0;
    }}
    p, label {{
      line-height: 1.55;
      color: #4b5563;
    }}
    label {{
      display: block;
      margin: 22px 0 8px;
      font-weight: 700;
      color: #111827;
    }}
    input {{
      box-sizing: border-box;
      width: 100%;
      min-height: 44px;
      padding: 0 12px;
      border: 1px solid #b8c1d1;
      border-radius: 6px;
      font-size: 16px;
    }}
    .actions {{
      display: flex;
      flex-wrap: wrap;
      gap: 10px;
      margin-top: 12px;
    }}
    button, a.button {{
      display: inline-flex;
      align-items: center;
      justify-content: center;
      min-height: 42px;
      padding: 0 16px;
      border: 1px solid #1f4f8f;
      border-radius: 6px;
      background: #2563eb;
      color: #ffffff;
      font-weight: 650;
      text-decoration: none;
      cursor: pointer;
      font-size: 15px;
    }}
    a.secondary {{
      background: #ffffff;
      color: #1f4f8f;
    }}
    code {{
      background: #eef2f7;
      padding: 2px 5px;
      border-radius: 4px;
    }}
  </style>
</head>
<body>
  <main>
    <h1>한글 명령으로 로컬 Excel 실행</h1>
    <p>
      아래 입력창에 <code>엑셀 실행해줘</code>처럼 한글로 요청하면,
      Rust 서버가 OpenAI API 모델 <code>{model}</code>로 명령을 판별한 뒤
      이 PC의 Windows Excel을 실행합니다. ActiveX는 사용하지 않습니다.
      Google Sheets 요청은 브라우저의 기존 Google 로그인 세션으로 새 스프레드시트를 엽니다.
      Microsoft 웹 Excel과 Office Web Viewer도 별도 버튼으로 실행할 수 있습니다.
    </p>

    <form method="post" action="/prompt">
      <label for="prompt">한글 프롬프트</label>
      <input id="prompt" name="prompt" value="엑셀 실행해줘" autocomplete="off">
      <div class="actions">
        <button type="submit">GPT로 판단해서 실행</button>
        <a class="button secondary" href="/open-excel">API 없이 바로 Excel 실행</a>
        <a class="button secondary" href="/open-csv">샘플 CSV를 Excel로 열기</a>
        <a class="button secondary" href="/open-google-sheets">Google Sheets 열기</a>
        <a class="button secondary" href="/open-ms-excel-web">MS Excel Web 열기</a>
        <a class="button secondary" href="/open-ms-office-viewer">MS Office Viewer 열기</a>
      </div>
    </form>

    <p style="margin-top:22px">
      상태 확인: <code>/health</code>
    </p>
  </main>
</body>
</html>
"#
    )
}

fn action_response(
    source: &str,
    action: &str,
    result: Result<String, String>,
) -> (&'static str, &'static str, String) {
    match result {
        Ok(message) => (
            "200 OK",
            "text/html; charset=utf-8",
            result_html(source, action, &message, None),
        ),
        Err(err) => (
            "500 Internal Server Error",
            "text/html; charset=utf-8",
            result_html(source, "실패", &err, None),
        ),
    }
}

fn prompt_response(prompt: Option<String>) -> (&'static str, &'static str, String) {
    let prompt = prompt.unwrap_or_default();
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return (
            "400 Bad Request",
            "text/html; charset=utf-8",
            result_html(
                "OpenAI 프롬프트",
                "실패",
                "프롬프트가 비어 있습니다.",
                Some(prompt),
            ),
        );
    }

    match classify_prompt_with_openai(prompt) {
        Ok(PromptAction::OpenExcel) => match open_blank_excel() {
            Ok(message) => (
                "200 OK",
                "text/html; charset=utf-8",
                result_html("OpenAI 프롬프트", "엑셀 실행", &message, Some(prompt)),
            ),
            Err(err) => (
                "500 Internal Server Error",
                "text/html; charset=utf-8",
                result_html("OpenAI 프롬프트", "실패", &err, Some(prompt)),
            ),
        },
        Ok(PromptAction::OpenCsv) => match create_csv_and_open() {
            Ok(message) => (
                "200 OK",
                "text/html; charset=utf-8",
                result_html(
                    "OpenAI 프롬프트",
                    "CSV 생성 후 엑셀 실행",
                    &message,
                    Some(prompt),
                ),
            ),
            Err(err) => (
                "500 Internal Server Error",
                "text/html; charset=utf-8",
                result_html("OpenAI 프롬프트", "실패", &err, Some(prompt)),
            ),
        },
        Ok(PromptAction::OpenGoogleSheets) => match open_google_sheets() {
            Ok(message) => (
                "200 OK",
                "text/html; charset=utf-8",
                result_html(
                    "OpenAI 프롬프트",
                    "Google Sheets 실행",
                    &message,
                    Some(prompt),
                ),
            ),
            Err(err) => (
                "500 Internal Server Error",
                "text/html; charset=utf-8",
                result_html("OpenAI 프롬프트", "실패", &err, Some(prompt)),
            ),
        },
        Ok(PromptAction::CreateGoogleSheets) => match open_google_sheets() {
            Ok(message) => (
                "200 OK",
                "text/html; charset=utf-8",
                result_html(
                    "OpenAI 프롬프트",
                    "Google Sheets 생성 화면 실행",
                    &message,
                    Some(prompt),
                ),
            ),
            Err(err) => (
                "500 Internal Server Error",
                "text/html; charset=utf-8",
                result_html("OpenAI 프롬프트", "실패", &err, Some(prompt)),
            ),
        },
        Ok(PromptAction::OpenMicrosoftExcelWeb) => match open_microsoft_excel_web() {
            Ok(message) => (
                "200 OK",
                "text/html; charset=utf-8",
                result_html(
                    "OpenAI 프롬프트",
                    "Microsoft Excel for the web 실행",
                    &message,
                    Some(prompt),
                ),
            ),
            Err(err) => (
                "500 Internal Server Error",
                "text/html; charset=utf-8",
                result_html("OpenAI 프롬프트", "실패", &err, Some(prompt)),
            ),
        },
        Ok(PromptAction::OpenMicrosoftOfficeViewer) => match open_microsoft_office_viewer(None) {
            Ok(message) => (
                "200 OK",
                "text/html; charset=utf-8",
                result_html(
                    "OpenAI 프롬프트",
                    "Microsoft Office Web Viewer 실행",
                    &message,
                    Some(prompt),
                ),
            ),
            Err(err) => (
                "500 Internal Server Error",
                "text/html; charset=utf-8",
                result_html("OpenAI 프롬프트", "실패", &err, Some(prompt)),
            ),
        },
        Ok(PromptAction::Deny) => (
            "200 OK",
            "text/html; charset=utf-8",
            result_html(
                "OpenAI 프롬프트",
                "실행 안 함",
                "Excel 실행 요청으로 판단되지 않아 로컬 프로그램을 실행하지 않았습니다.",
                Some(prompt),
            ),
        ),
        Err(err) => (
            "500 Internal Server Error",
            "text/html; charset=utf-8",
            result_html("OpenAI 프롬프트", "API 실패", &err, Some(prompt)),
        ),
    }
}

fn result_html(source: &str, action: &str, message: &str, prompt: Option<&str>) -> String {
    let prompt_line = prompt
        .map(|p| {
            format!(
                "<p><strong>입력:</strong> <code>{}</code></p>",
                html_escape(p)
            )
        })
        .unwrap_or_default();
    format!(
        r#"<!doctype html>
<html lang="ko">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>실행 결과</title>
  <style>
    body {{
      margin: 0;
      font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", "Malgun Gothic", sans-serif;
      color: #111827;
      background: #f5f7fb;
    }}
    main {{
      width: min(780px, calc(100vw - 32px));
      margin: 48px auto;
      background: #ffffff;
      border: 1px solid #d7dde8;
      border-radius: 8px;
      padding: 28px;
      box-shadow: 0 16px 36px rgba(31, 41, 55, 0.08);
    }}
    h1 {{ margin: 0 0 10px; font-size: 26px; letter-spacing: 0; }}
    p {{ line-height: 1.55; color: #4b5563; }}
    code {{ background: #eef2f7; padding: 2px 5px; border-radius: 4px; }}
    pre {{
      white-space: pre-wrap;
      overflow-wrap: anywhere;
      background: #111827;
      color: #f9fafb;
      padding: 14px;
      border-radius: 6px;
    }}
    a {{
      display: inline-flex;
      align-items: center;
      justify-content: center;
      min-height: 40px;
      padding: 0 14px;
      border-radius: 6px;
      background: #2563eb;
      color: white;
      text-decoration: none;
      font-weight: 650;
    }}
  </style>
</head>
<body>
  <main>
    <h1>{action}</h1>
    <p><strong>경로:</strong> {source}</p>
    {prompt_line}
    <pre>{message}</pre>
    <a href="/">돌아가기</a>
  </main>
</body>
</html>
"#,
        source = html_escape(source),
        action = html_escape(action),
        prompt_line = prompt_line,
        message = html_escape(message),
    )
}

fn classify_prompt_with_openai(prompt: &str) -> Result<PromptAction, String> {
    let api_key = env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY 환경변수가 설정되어 있지 않습니다.".to_string())?;
    let model = openai_model();

    let instructions = "\
한국어 명령 분류기. 한 단어만 출력: OPEN_EXCEL, OPEN_CSV, OPEN_GOOGLE_SHEETS, CREATE_GOOGLE_SHEETS, OPEN_MICROSOFT_EXCEL_WEB, OPEN_MICROSOFT_OFFICE_VIEWER, DENY. \
엑셀 실행/열기/켜줘는 OPEN_EXCEL. CSV/표 데이터/샘플 CSV는 OPEN_CSV. \
구글 스프레드시트/구글시트/Google Sheets/웹 스프레드시트/웹엑셀을 열기/실행/켜기는 OPEN_GOOGLE_SHEETS. \
구글 시트를 새로 만들기/생성/샘플 데이터 채워서 만들기는 CREATE_GOOGLE_SHEETS. \
마이크로소프트 웹용 엑셀/Excel for the web/Microsoft 365 Excel/MS 웹엑셀은 OPEN_MICROSOFT_EXCEL_WEB. \
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
    } else if decision_area.contains("CREATE_GOOGLE_SHEETS") {
        Ok(PromptAction::CreateGoogleSheets)
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
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "25",
            "https://api.openai.com/v1/responses",
            "-H",
            &auth,
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            "@-",
            "-w",
            "\n__HTTP_STATUS__:%{http_code}",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("curl 실행 실패: {err}"))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(payload.as_bytes())
            .map_err(|err| format!("curl stdin 쓰기 실패: {err}"))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|err| format!("curl 대기 실패: {err}"))?;
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

fn openai_model() -> String {
    env::var("OPENAI_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "gpt-5.5".to_string())
}

fn open_blank_excel() -> Result<String, String> {
    spawn_powershell("Start-Process -FilePath 'excel.exe'")?;
    Ok("Windows Excel 실행 요청을 보냈습니다.\n".to_string())
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
        "CSV를 만들고 Excel 실행 요청을 보냈습니다.\nWSL: {csv_path:?}\nWindows: {win_path}\n"
    ))
}

fn open_google_sheets() -> Result<String, String> {
    let url = google_sheets_url();
    open_browser_url(&url)?;
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

fn demo_csv_path() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let filename = format!("web-excel-launcher-demo-{stamp}.csv");
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

fn url_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
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

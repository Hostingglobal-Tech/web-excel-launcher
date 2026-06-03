# Web Excel Launcher

한글 프롬프트로 Windows Excel을 실행하는 Rust 데모입니다.

이 저장소는 두 가지를 보여줍니다.

1. 로컬 웹앱이 로컬 Windows Excel을 실행할 수 있다.
2. 리모트 Vultr 서버가 Tailscale을 통해 로컬 PC의 Excel 실행 agent를 호출할 수 있다.
3. 같은 allowlist 구조로 Google Sheets 새 스프레드시트도 브라우저의 기존 Google 로그인 세션에서 열 수 있다.
4. Vultr gateway가 공개 샘플 `.xlsx`를 제공하고, local agent가 로컬 PC 브라우저에서 Microsoft Excel for the web 또는 Microsoft Office Web Viewer를 열 수 있다.

브라우저가 직접 로컬 프로그램을 실행하는 방식이 아닙니다. 브라우저는 HTTP 요청만 보내고, Rust 서버 또는 local agent가 allowlist된 실행 작업을 수행합니다. ActiveX는 사용하지 않습니다. Google Sheets 실행은 로컬 프로그램 의존이 아니라 기본 브라우저의 인증된 Google 세션으로 `https://docs.google.com/spreadsheets/create`를 여는 웹 스프레드시트 경로입니다.

Microsoft 경로도 분리했습니다.

- Microsoft Excel for the web: 기본 URL `https://www.microsoft365.com/launch/excel?auth=1`
- Microsoft Office Web Viewer: `https://view.officeapps.live.com/op/view.aspx?src=<public-xlsx-url>`

Office Web Viewer는 Microsoft 서버가 접근 가능한 공개 문서 URL만 볼 수 있습니다. 그래서 gateway는 `/sample.xlsx`를 공개로 제공하고, local agent는 그 URL을 Office Web Viewer에 넘겨 로컬 PC 브라우저에서 웹 뷰어를 띄웁니다.

## Architecture

### Local-only mode

```text
Windows browser
  -> WSL Rust HTTP server
  -> powershell.exe
  -> Windows excel.exe
```

파일:

- `src/main.rs`

### Remote gateway mode

```text
Browser
  -> Vultr gateway
  -> Tailscale private network
  -> WSL local agent
  -> Windows excel.exe
```

파일:

- `src/tailscale_excel_bridge.rs`

중요한 경계:

- Vultr는 Excel을 직접 실행하지 않습니다.
- Vultr는 Tailscale 내부 주소의 local agent에 요청만 전달합니다.
- 실제 `excel.exe` 실행은 Windows PC 옆에 있는 WSL local agent가 수행합니다.
- `OPENAI_API_KEY`는 Vultr에 둘 필요가 없습니다. local agent에서만 사용하면 됩니다.

## OpenAI Modes

### Prompt classification mode

한글 요청을 OpenAI Responses API로 짧게 분류합니다.

```text
엑셀 실행해줘 -> OPEN_EXCEL
샘플 CSV 만들어서 엑셀로 열어줘 -> OPEN_CSV
구글 스프레드시트 열어줘 -> OPEN_GOOGLE_SHEETS
MS 웹용 엑셀 열어줘 -> OPEN_MICROSOFT_EXCEL_WEB
MS 엑셀 웹 뷰어로 봐줘 -> OPEN_MICROSOFT_OFFICE_VIEWER
기타 요청 -> DENY
```

설정:

- model: `gpt-5.5`
- `store=false`
- `reasoning.effort=none`
- `max_output_tokens=64`

이 모드는 빠르고 결정적입니다. Excel 실행 자체는 local agent allowlist 함수가 수행합니다.

### Computer Use mode

OpenAI Computer Use를 사용해 화면 기반 GUI 조작 루프를 실행합니다.

```text
gpt-5.5 + tools: [{ "type": "computer" }]
```

local agent가 수행하는 일:

1. Windows 화면 캡처
2. OpenAI에 screenshot 전달
3. 모델이 반환한 `screenshot`, `keypress`, `type`, `click`, `wait` 액션 실행
4. 새 screenshot을 다시 전달
5. `computer_call`이 끝날 때까지 반복

Computer Use 모드는 `previous_response_id` 기반 루프가 필요하므로 이 모드에서만 `store=true`를 사용합니다.

## Security

저장소에 넣지 않는 값:

- `OPENAI_API_KEY`
- `EXCEL_BRIDGE_TOKEN`
- `.env`
- 빌드된 바이너리
- 로그 파일

권장 사항:

- local agent는 Tailscale IP에만 바인딩하십시오.
- 공개 인터넷에 local agent를 노출하지 마십시오.
- bridge token은 긴 랜덤 값으로 설정하십시오.
- Computer Use는 화면을 보고 키보드/마우스 액션을 실행하므로 개인 PC나 인증된 업무 화면에서는 사람이 감시하는 상태로 쓰십시오.

## Local-only Build

Rust 표준 라이브러리만 사용하므로 Cargo 없이 `rustc`로 빌드할 수 있습니다.

```bash
rustc src/main.rs -O -o web-excel-launcher
```

실행:

```bash
export OPENAI_MODEL='gpt-5.5'
./web-excel-launcher 0.0.0.0:8877
```

실행 전 현재 셸에 `OPENAI_API_KEY` 환경변수가 설정되어 있어야 합니다.

접속:

```text
http://wsl:8877/
```

또는 환경에 따라:

```text
http://localhost:8877/
```

Local-only endpoints:

- `GET /`
- `GET /health`
- `GET /open-excel`
- `GET /open-csv`
- `GET /open-google-sheets`
- `GET /open-ms-excel-web`
- `GET /open-ms-office-viewer`
- `POST /prompt`

Google Sheets URL은 기본값으로 `https://docs.google.com/spreadsheets/create`를 사용합니다. 특정 문서나 Google Workspace 경로를 열고 싶으면 실행 환경에서 `GOOGLE_SHEETS_URL`을 지정하십시오.
Microsoft Excel for the web URL은 `MS_EXCEL_WEB_URL`로 바꿀 수 있습니다. Office Web Viewer는 `MS_OFFICE_VIEWER_SRC_URL` 또는 `src` query parameter가 필요합니다.

## Vultr + Tailscale Build

양쪽 노드에서 같은 Rust 파일을 빌드합니다.

```bash
rustc src/tailscale_excel_bridge.rs -O -o tailscale-excel-bridge
```

### 1. Local agent on WSL

local agent는 로컬 PC의 WSL에서 실행합니다.

```bash
export EXCEL_BRIDGE_TOKEN='set-a-private-random-token'
export OPENAI_MODEL='gpt-5.5'
./tailscale-excel-bridge agent 100.x.x.x:8788
```

실행 전 현재 셸에 `OPENAI_API_KEY` 환경변수가 설정되어 있어야 합니다.

health check:

```bash
curl http://100.x.x.x:8788/health
```

예상 응답:

```text
agent ok
```

### 2. Gateway on Vultr

gateway는 Vultr 같은 리모트 Linux 노드에서 실행합니다.

```bash
export EXCEL_BRIDGE_TOKEN='same-private-random-token'
./tailscale-excel-bridge gateway 100.y.y.y:8878 http://100.x.x.x:8788
```

health check:

```bash
curl http://100.y.y.y:8878/health
```

예상 응답:

```text
gateway ok
```

접속:

```text
http://100.y.y.y:8878/
```

Gateway endpoints:

- `POST /prompt`
- `POST /prompt-computer`
- `POST /open-google-sheets`
- `POST /open-ms-excel-web`
- `POST /open-ms-office-viewer`
- `GET /sample.xlsx`

## Systemd User Services

`deploy/`에는 재부팅 후에도 agent/gateway가 다시 올라오도록 하는 user systemd 템플릿이 들어 있습니다.

Local agent:

```bash
install -D -m 644 deploy/tailscale-excel-agent.service ~/.config/systemd/user/tailscale-excel-agent.service
install -D -m 600 deploy/tailscale-excel-agent.env.example ~/.config/tailscale-excel-agent.env
systemctl --user daemon-reload
systemctl --user enable --now tailscale-excel-agent.service
```

Gateway:

```bash
install -D -m 644 deploy/tailscale-excel-gateway.service ~/.config/systemd/user/tailscale-excel-gateway.service
install -D -m 600 deploy/tailscale-excel-gateway.env.example ~/.config/tailscale-excel-gateway.env
systemctl --user daemon-reload
systemctl --user enable --now tailscale-excel-gateway.service
```

실제 운영 전에는 env 파일의 placeholder를 개인 Tailnet IP, bridge token, API key 값으로 바꾸십시오.

## Verification Examples

Prompt classification mode:

```bash
curl -X POST http://100.y.y.y:8878/prompt \
  --data-urlencode 'prompt=엑셀 실행해줘'
```

성공 로그 예:

```text
agent action=OPEN_EXCEL
prompt=엑셀 실행해줘
로컬 Windows Excel 실행 요청을 보냈습니다.
```

Google Sheets mode:

```bash
curl -X POST http://100.y.y.y:8878/open-google-sheets
```

성공 로그 예:

```text
agent action=OPEN_GOOGLE_SHEETS
Google Sheets 새 스프레드시트 실행 요청을 보냈습니다.
URL: https://docs.google.com/spreadsheets/create
기존 브라우저의 Google 로그인 세션을 사용합니다.
```

Microsoft Excel for the web mode:

```bash
curl -X POST http://100.y.y.y:8878/open-ms-excel-web
```

Microsoft Office Web Viewer mode:

```bash
curl -X POST http://100.y.y.y:8878/open-ms-office-viewer
```

이때 gateway는 기본적으로 자기 공개 URL의 `/sample.xlsx`를 viewer source로 넘깁니다. 공인 URL이 자동으로 잡히지 않는 환경에서는 gateway env에 다음 값을 지정하십시오.

```bash
EXCEL_GATEWAY_PATH_PREFIX=/web-excel
EXCEL_GATEWAY_PUBLIC_URL=https://ntopng.nmsglobal.kr/web-excel
```

그리고 local agent env에는 같은 샘플 URL을 지정할 수 있습니다.

```bash
MS_OFFICE_VIEWER_SRC_URL=https://ntopng.nmsglobal.kr/web-excel/sample.xlsx
```

Vultr에서 이미 Caddy가 80/443을 담당하는 경우에는 새 공개 포트를 열지 말고 Caddy에 다음 path proxy만 추가하면 됩니다.

```caddy
handle /web-excel* {
    reverse_proxy http://127.0.0.1:8890
}
```

## Microsoft References

- Excel for the web supports browser-based workbook work and common workbook formats: <https://support.microsoft.com/en-us/office/differences-between-using-a-workbook-in-the-browser-and-in-excel-f0dc28ed-b85d-4e1d-be6d-5878005db3b6>
- Microsoft documents creating workbooks in Excel for the web from Microsoft 365 Home or OneDrive: <https://support.microsoft.com/en-us/office/quick-tips-get-work-done-with-excel-for-the-web-49a8a468-227f-417b-92c4-fd247a93a62d>
- Microsoft Q&A references the Office Web Viewer URL shape `view.officeapps.live.com/op/view.aspx?src=...`: <https://learn.microsoft.com/en-us/answers/questions/5130657/view-officeapps-live-com-access-error>

Computer Use mode:

```bash
curl -X POST http://100.y.y.y:8878/prompt-computer \
  --data-urlencode 'prompt=엑셀 실행해줘'
```

성공 로그 예:

```text
mode=OpenAI computer tool
model=gpt-5.5
store=true (computer loop previous_response_id 필요)
step=1 action=screenshot
step=2 action=keypress WIN+R
step=2 action=type excel
step=2 action=keypress ENTER
verify=EXCEL.EXE 감지됨
```

Windows 프로세스 확인:

```powershell
Get-Process -Name EXCEL -ErrorAction SilentlyContinue
```

## Example Prompts

```text
엑셀 실행해줘
```

```text
샘플 CSV 만들어서 엑셀로 열어줘
```

## Notes

이 데모의 핵심은 리모트와 로컬의 역할 분리입니다.

- 리모트 Vultr: 웹 UI와 gateway
- Tailscale: private transport
- 로컬 WSL agent: OpenAI API 호출, 화면 캡처, allowlist 실행
- Windows host: 실제 Excel 프로세스 실행

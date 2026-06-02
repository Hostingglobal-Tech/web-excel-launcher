# Web Excel Launcher

한글 프롬프트로 로컬 Windows Excel을 실행하는 작은 Rust 데모입니다.

브라우저가 직접 로컬 프로그램을 실행하는 방식이 아니라, 로컬 Rust HTTP 서버가 웹 요청을 받고 Windows `excel.exe`를 실행합니다. ActiveX는 사용하지 않습니다.

## 기능

- 한글 UI 제공
- `엑셀 실행해줘` 같은 프롬프트를 OpenAI Responses API로 분류
- 모델 기본값: `gpt-5.5`
- 분류 결과가 `OPEN_EXCEL`이면 로컬 Windows Excel 실행
- 샘플 CSV 생성 후 Excel로 열기 지원
- API 응답 저장 방지: `store=false`
- 출력 비용 제한: `max_output_tokens=64`, `reasoning.effort=none`

## 보안

`OPENAI_API_KEY` 값은 소스코드나 저장소에 넣지 않습니다. 실행 환경변수로만 설정하십시오.

이 도구는 로컬 데모/개인 PC용입니다. 공개 인터넷에 노출하지 마십시오.

## 빌드

Rust 표준 라이브러리만 사용하므로 Cargo 없이 `rustc`로 빌드할 수 있습니다.

```bash
rustc src/main.rs -O -o web-excel-launcher
```

## 실행

```bash
export OPENAI_MODEL='gpt-5.5'
./web-excel-launcher 0.0.0.0:8877
```

실행 전 현재 셸에 `OPENAI_API_KEY` 환경변수가 설정되어 있어야 합니다.

WSL에서 실행한 뒤 Windows 브라우저에서 접속합니다.

```text
http://wsl:8877/
```

또는 환경에 따라 다음 주소도 동작합니다.

```text
http://localhost:8877/
```

## 엔드포인트

- `GET /` 한글 웹 UI
- `GET /health` 상태 확인
- `GET /open-excel` API 없이 Excel 바로 실행
- `GET /open-csv` 샘플 CSV 생성 후 Excel 실행
- `POST /prompt` 한글 프롬프트를 OpenAI API로 분류 후 실행

## 예시 프롬프트

```text
엑셀 실행해줘
```

```text
샘플 CSV 만들어서 엑셀로 열어줘
```

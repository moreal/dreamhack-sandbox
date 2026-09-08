# Multi-OS VM Feature PRD & Implementation Plan

> **Claude 인계용:** 사용자는 이 기능의 구현을 Claude에게 일임했다. 이 문서를 요구사항과 작업 순서의 기준으로 사용한다. 사용 가능한 경우 `superpowers:executing-plans`로 단계별 구현·검증을 진행한다. 체크박스는 검증 후에만 완료 표시한다.

**Goal:** macOS 호스트에서 macOS·Windows·Kali Linux VM을 생성하고, 사용자 도구를 추가한 재사용 이미지를 빌드하며, x86_64 게스트를 `emulate` 모드로 실행한다.

**Architecture:** 기존 Tart 경로를 유지하고 UTM/QEMU adapter를 추가한다. 프로필과 저장된 세션으로 백엔드·실행 모드를 결정하며, OS별 프로비저닝과 게스트 연결을 공통 애플리케이션 서비스에서 사용한다.

**Tech Stack:** 기존 Rust workspace, tokio, clap, ratatui, serde, ProcessRunner, Nix. 추가 런타임은 UTM의 `utmctl`·AppleScript 자동화이며 디스크 변환에 필요한 경우에만 `qemu-img`를 사용한다.

**Spec:** 이 문서의 §1–§7이 기능 PRD, §8–§10이 구현 및 검증 계획이다. 기존 구조는 `DESIGN.md`, 현재 사용법은 `README.md`, 실제 MVP 검증 기록은 `docs/verification.md`를 함께 읽는다.

**Status:** 구현 전 / 2026-09-08 작성. 이 문서 작성에는 코드 구현, 도구 설치, 실제 VM 생성이 포함되지 않았다.

**이전 계획:** 완료된 MVP의 기존 `PLAN.md`는 [docs/archive/2026-09-05-mvp-plan.md](docs/archive/2026-09-05-mvp-plan.md)에 보존했다. `.claude/workflows/mvp-build.js`는 과거 Task 1–9 전용이므로 새 계획 실행에 그대로 사용하지 않는다. 기존 문서에서 MVP 작업을 뜻하는 `PLAN.md` 참조는 보관본을 가리키는 것으로 해석한다.

## 1. 사용자 요구와 범위

사용자의 요구: “필요한 도구를 추가해서 VM을 만들 수 있어야 하며, macOS뿐 아니라 Windows와 Kali Linux도 실행하고, emulate 모드로 x86도 지원해야 한다.”

이 PRD의 Windows 지원은 **Windows 게스트** 지원이다. Windows 호스트용 포팅은 범위 밖이다. x86의 필수 범위는 **x86_64 전체 시스템 에뮬레이션**이며 32비트 i386 전용 게스트는 후속 범위로 둔다. Rosetta·Wine·컨테이너로 전체 VM 요구를 대체하지 않는다.

### 지원 매트릭스

| 게스트 | 아키텍처 | 모드 | 백엔드 | 출시 기준 |
|---|---|---|---|---|
| 기존 macOS | arm64 | virtualize | Tart | 기존 두 프로필 회귀 검증 필수 |
| Kali Linux | arm64 | virtualize | UTM/QEMU + 하드웨어 가속 | 필수 |
| Kali Linux | x86_64 | emulate | UTM/QEMU, hypervisor off | 필수 |
| Windows 11 | x86_64 | emulate | UTM/QEMU, hypervisor off | 필수 |
| Windows 11 | arm64 | virtualize | UTM/QEMU + 하드웨어 가속 | 후속 기능 |

- 호스트 기준은 macOS / Apple Silicon이다. macOS·UTM 최소 지원 버전은 Task 1에서 실제 API 확인 후 정확한 버전으로 기록하고 검사한다.
- 같은 아키텍처라도 `emulate`를 지정하면 소프트웨어 에뮬레이션을 사용한다. `virtualize` 불가 시 모드를 조용히 바꾸지 않는다.
- 첫 OS 설치는 UTM 콘솔을 통한 수동 설치를 허용한다. ISO 연결과 VM 생성만으로 이미지 빌드 성공을 표시하지 않는다.
- macOS x86_64 에뮬레이션, GPU 가속 보장, 스냅샷 UI, 원격 호스트, 범용 무인 OS 설치 엔진은 범위 밖이다.
- 프로필 YAML만 추가하거나 fake 테스트만 통과한 상태는 기능 완료가 아니다.

### 성공하는 사용자 흐름

1. 프로필에서 OS·아키텍처·모드·리소스를 선택한다.
2. 설치 ISO로 새 VM을 만들거나 설치된 UTM VM을 원본으로 복제한다.
3. OS 설치와 게스트 연결을 준비한 후 사용자 도구 설치 스크립트를 실행해 재사용 이미지를 만든다.
4. 해당 프로필로 Dreamhack 문제를 일회용 VM에 배치하고 콘솔·셸로 접근한다.
5. 중지·재개·삭제 후에도 원본 이미지와 다른 사용자의 VM은 보존된다.

## 2. 확인된 현재 구조

| 현재 파일 | 확인된 제약 | 필요한 변화 |
|---|---|---|
| `crates/wargame-core/src/domain/profile.rs` | Linux/Windows와 x86_64 자료형은 있으나 backend/mode 없음 | 기존 YAML과 호환되는 선택 필드 |
| `crates/wargame-core/src/domain/compat.rs` | OS 집합 × arch 집합으로 지원 표현 | 실제 OS/arch/mode 조합 검증 |
| `crates/wargame-core/src/ports/vm_provider.rs` | VmSpec은 이미지·리소스 중심 | 선택된 실행 설정 전달 |
| `crates/wargame-core/src/app/mod.rs` | AppContext에 provider/transport가 각각 하나 | 프로필·세션별 선택 |
| `crates/wargame-cli/src/wiring.rs` | Tart 고정, SSH 사용자 admin 고정 | backend registry와 guest 설정 기반 연결 |
| `crates/wargame-tart/src/build.rs` | Tart와 Bash/SSH 키 설치에 종속 | 공통 빌드 흐름과 OS/backend 세부 처리 분리 |
| `crates/wargame-core/src/infra/ssh.rs` | IP 중심 연결 | 사용자·포트·게스트 셸 반영 |
| `guest/bootstrap.sh` | POSIX 도구·경로 가정 | Kali 검증과 Windows PowerShell 구현 |
| `crates/wargame-core/src/domain/session.rs` | backend는 저장하나 mode/user 등 부족 | 재개용 실행 설정 스냅샷 |

Tart 자체는 macOS와 Linux를 지원하지만 현재 프로젝트의 실제 경로는 macOS에 맞춰져 있다. 이번에는 Kali를 UTM으로 통일해 에뮬레이션과 같은 관리 흐름을 제공한다.

## 3. 기능 요구사항

### FR-1. 프로필 및 호환성

- VM 설정에 `backend: auto|tart|utm`, `mode: virtualize|emulate`를 추가한다.
- 기존 `wargame.vm/v1` macOS YAML은 수정 없이 Tart/virtualize로 해석한다. 기존 기본 프로필을 유지한다.
- `auto`는 macOS/arm64/virtualize → Tart, 지원되는 Kali·Windows 조합 → UTM으로 결정한다. 실행 중 다른 백엔드로 fallback하지 않는다.
- `os: linux`는 유지하고 `distribution: kali`로 구분한다. arch 이름은 `arm64`, `x86_64`를 사용한다.
- 양수 리소스, 메모리 변환 overflow, 경로, 지원 조합을 VM 생성 전에 검사한다.
- profiles 및 TUI 선택 화면에 OS/arch/backend/mode와 이미지 준비 상태를 표시한다.
- 불가 조합은 요청값·불가 이유·사용 가능한 조합을 포함한 오류로 반환한다.

### FR-2. VM 생성과 이미지 소스

- 기존 Tart OCI/base image 입력을 유지한다.
- UTM 입력은 로컬 설치 ISO 또는 설치된 로컬 UTM VM UUID이다. 기존 원본은 복제만 하며 원본에서 프로비저닝하지 않는다.
- `profile init`은 내장 템플릿을 사용자 이름으로 복사한다. `profile create --iso`는 빈 디스크와 설치 미디어를 갖는 설치용 VM을 만든다.
- 설치용 UUID, 콘솔 여는 방법, OS 설치 후 SSH/guest tools 준비, build 재개 명령을 출력한다.
- 설치 진행은 앱 소유 build record에 저장한다. 같은 명령을 다시 실행하면 설치 VM을 재사용·안내하고 중복 VM/디스크를 만들지 않는다.
- `.utm` 번들 import, 원격 이미지 자동 다운로드는 후속 기능이다.
- ISO 존재·읽기 가능 여부를 검사한다. 사용자가 제공한 SHA-256은 검증한다. 확인되지 않은 ISO OS/arch를 검증 완료로 표시하지 않는다.
- Windows ISO는 사용자가 제공한다. 라이선스 구매·자동 활성화·설치 요건 우회는 구현하지 않는다. 선택한 Windows 버전에 필요한 펌웨어·TPM·드라이버 구성을 실제 설치로 검증한다.

### FR-3. 사용자 도구와 이미지 빌드

- 기존 `provision.scripts`, `tools.verify`를 유지한다. 스크립트는 프로필 디렉터리 기준으로 찾아 게스트에 복사한다.
- macOS/Kali는 Bash, Windows는 PowerShell로 실행한다. 설치 스크립트는 반드시 게스트에서 실행한다.
- 사용자 스크립트를 추가하고 build를 재실행하면 도구를 포함한 새 재사용 이미지를 생성한다.
- 사용자 편집 프로필·사용자 스크립트를 내장 파일 배포 과정에서 덮어쓰지 않는다.
- Kali 기본 도구: Python 3, pip/venv, Git, curl, unzip, file, GDB, binutils, build-essential.
- Windows 기본 도구: PowerShell, OpenSSH Server, Python 3, Git, 7-Zip. 설치 출처와 검증 명령을 스크립트에 명시한다.
- NAT에서 의존성을 설치하고 단계·종료 코드·실패한 검증 명령을 로그에 남긴다. 설치 중 재부팅 필요 상태를 처리하고 다시 연결한다.
- 모든 검증 및 정상 종료 후에만 이미지를 ready로 게시한다. 새 빌드 실패 시 이전 정상 이미지를 보존한다.
- 소스, OS/arch/mode, 프로필·스크립트 해시, 검증 결과를 build record에 저장한다. 프로필 변경과 이미지 불일치는 재빌드를 안내한다.

### FR-4. 게스트 연결과 문제 배치

- 게스트 접속은 우선 SSH/SFTP로 통일하되 Windows 명령은 PowerShell에 맞게 인코딩한다. UTM guest agent는 주소 조회 등 필요한 기능에 사용한다.
- endpoint에는 host, port, user, key 경로를 포함한다. admin을 고정하지 않는다. 포트 포워딩을 쓰면 세션마다 충돌 없는 포트를 할당하고 저장한다.
- Windows OpenSSH/PowerShell 및 UTM guest tools 준비 절차를 제공한다. 서비스 미준비를 성공으로 처리하지 않는다.
- Windows 경로, 공백, 작은따옴표·큰따옴표·한글 경로를 지원한다. Unix shell quoting을 Windows에 재사용하지 않는다.
- `incoming`, `work`, `notes`, `output`과 문제 메타데이터를 만든다. 게스트 파일 해시는 호스트 staging 해시와 같아야 한다.
- 압축 해제는 게스트에서만 수행한다. 절대 경로, `..`, symlink/hardlink 및 Windows 경로 우회가 작업 디렉터리 밖에 쓰지 못하도록 검증한다. 안전한 처리가 불가능한 포맷은 원본을 유지하고 이유를 표시한다.
- 문제 파일을 자동 실행하지 않는다. 호스트에서는 문제 파일을 압축 해제·마운트·미리보기·실행하지 않는다.

### FR-5. 생명주기와 이전 데이터 호환성

- launch/inspect/list/stop/resume/delete/prune/gui/ssh를 CLI/TUI 공통 서비스로 제공한다.
- 과거 세션 JSON에 migration/default를 정의한다. 과거 세션의 누락값은 기존 Tart/arm64/virtualize/admin 동작으로 복원한다.
- 새 세션은 backend, 안정적인 VM ID, mode, guest user, 연결 설정, workspace를 저장한다. 프로필 편집·기본 백엔드 변경 후에도 저장된 설정으로 재개한다.
- UTM UUID를 provider ID로 사용하고 표시 이름과 구분한다. 다른 백엔드의 같은 VM 이름을 혼동하지 않는다.
- UTM 창을 닫거나 앱 밖에서 VM을 중지한 경우 실제 상태와 세션을 재조정한다.
- 앱 기록의 UUID와 소유권을 대조한다. 이름 prefix만으로 외부 VM을 삭제하지 않는다. 사용자 VM `kindle-vm`과 원본 VM은 수정·중지·삭제하지 않는다.
- 부팅·명령·중지 timeout을 구분한다. emulate 부팅 기본값은 1800초, virtualize는 기존 300초를 기준으로 override를 지원한다. 수동 OS 설치 대기는 별도 상태이다.
- 실패 시 단계별 정리와 정리 실패를 기록한다. 설치용 VM은 재개할 수 있도록 보존하고 문제 세션 복제본은 기존 keep 정책에 따른다.

### FR-6. 네트워크와 의존성

- 기본 실행 네트워크는 기존 `isolated`, 이미지 빌드는 NAT이다.
- UTM isolated는 외부 통신을 막고 호스트 관리 연결을 허용해야 한다. 다른 LAN 장치·인터넷 접근 차단을 실제 VM으로 검사한다.
- 해당 구성에서 isolated가 불가능하면 VM 시작 전에 거부한다. NAT로 자동 변경하지 않는다. 사용자가 `--network nat`을 지정하면 실행한다.
- Tart macOS 실행에 UTM 설치를 요구하지 않는다.
- doctor에 `--profile`을 추가한다. 필요한 backend, 실제 경로·버전, 자동화 권한, 이미지 준비, 디스크 용량, 연결 도구 및 네트워크 지원을 검사한다.
- utmctl 탐색 순서: 설정 경로 → PATH → `/Applications/UTM.app/Contents/MacOS/utmctl`. 공백 포함 경로를 지원한다.
- 누락 도구는 공식 설치 경로와 재검증 방법을 안내한다. 일반 실행 명령이 패키지 설치·권한 변경을 몰래 수행하지 않는다. 구현자는 필요한 개발·검증 도구를 설치할 수 있으나 환경 권한 정책을 따른다.
- Nix가 UTM.app을 제공한다고 가정하지 않는다. 빌드 의존성과 macOS 앱 런타임 의존성을 구분한다.

## 4. 목표 CLI와 프로필 계약

다음은 새로 구현할 인터페이스다. 현재 지원되는 명령이라고 가정하지 않는다. 기존 명령과 alias는 보존한다.

```bash
# ISO 기반 Kali 설치
wargame profile init kali-x86_64 --name kali-custom
wargame profile create kali-custom --iso /path/to/kali-amd64.iso
wargame profile console kali-custom
# 콘솔에서 OS 설치·SSH/guest tools 준비, 프로필에 사용자 스크립트 추가
wargame profile build kali-custom
wargame doctor --profile kali-custom
wargame launch dreamhack:15 --profile kali-custom

# ISO 기반 Windows 설치
wargame profile init windows-x86_64 --name windows-custom
wargame profile create windows-custom --iso /path/to/windows-x64.iso
wargame profile console windows-custom
wargame profile build windows-custom

# ISO 대신 이미 설치된 UTM VM을 복제하는 대안
wargame profile init windows-x86_64 --name windows-from-base
wargame profile create windows-from-base --from-vm <utm-uuid>
wargame profile build windows-from-base
```

- init은 기존 이름과 충돌하면 중단한다. `--iso`, `--from-vm`은 상호 배타적이다.
- 연결된 설치 VM이 있는 프로필에 다른 소스를 지정하면 충돌 오류와 새 프로필 생성 방법을 안내한다.
- create 성공은 설치용 VM 생성/복제 성공, build 성공은 게스트 준비와 검증을 마친 재사용 이미지 생성 성공이다.
- TUI는 프로필 선택·문제 실행·세션 관리를 지원한다. 첫 ISO 설치 마법사는 CLI+UTM 콘솔로 제공해도 된다.

프로필 확장 예시:

```yaml
apiVersion: wargame.vm/v1
kind: Profile
metadata:
  name: kali-custom
  description: Kali with custom analysis tools
vm:
  backend: utm
  mode: emulate
guest:
  os: linux
  distribution: kali
  arch: x86_64
  user: kali
image:
  base: "" # UTM은 create의 build record 사용; Tart에서는 기존 base 필수
resources:
  cpu: 4
  memory_gib: 8
  disk_gib: 80
provision:
  scripts:
    - scripts/kali/common.sh
    - scripts/custom/my-tools.sh
tools:
  verify:
    - python3 --version
    - gdb --version
workspace:
  root: /home/kali/wargame
```

- ISO 경로·원본 UUID·설치 진행은 build record에 저장한다. YAML의 빈 UTM base는 build record가 있어야만 build/launch 가능하다는 뜻이다.
- Windows 템플릿은 `os: windows`, `arch: x86_64`, `mode: emulate`, user `wargame`, workspace `C:\wargame`, 4 CPU/8 GiB RAM/80 GiB disk, `scripts/windows/common.ps1`을 사용한다. 설치 계정과 user를 일치시킨다.
- Kali arm64 템플릿은 같은 리소스에서 `arch: arm64`, `mode: virtualize`로 제공한다.

## 5. 설계 방향과 경계

**선택: Tart + UTM adapter.** 기존 macOS 경로를 보존하고 UTM에서 Windows/Kali와 콘솔을 관리한다. 실제 utmctl 명령을 먼저 확인하고 생성·복제·설정은 공식 AppleScript API로 보완한다. 문서에 없는 CLI 명령을 추측하지 않는다.

**대안: Tart + 직접 QEMU.** 헤드리스 제어는 단순해질 수 있지만 디스플레이, QMP, 펌웨어, TPM과 프로세스 관리를 직접 맡아야 한다. UTM에서 필수 요구가 불가능하다는 재현 증거가 있을 때만 검토한다. 새 백엔드를 둘 다 구현하는 것은 범위 밖이다.

1. backend resolver는 launch에는 profile, 기존 세션 작업에는 저장된 backend/VM ID를 사용한다. 전역 단일 provider에 우회 분기를 누적하지 않는다.
2. 새 `wargame-utm` crate는 VM 생성·복제·구성·시작·상태·종료·삭제·콘솔을 담당하며 Dreamhack에 의존하지 않는다.
3. endpoint 및 명령 전략은 OS/세션별로 정한다. 앱 서비스가 utmctl/AppleScript를 직접 호출하지 않는다.
4. 공통 이미지 빌드는 준비→복제→부팅→연결→프로비저닝→검증→정상 종료→게시 순서를 갖는다. Tart 세부 처리는 Tart adapter에 남긴다.
5. 설치 build record와 문제 session record를 분리한다. 소유 UUID·단계·오류를 저장해 재실행 시 복구한다.
6. 외부 프로세스는 ProcessRunner를 사용한다. 호스트 shell 문자열 조합을 금지한다. AppleScript/게스트 스크립트는 신뢰된 템플릿에 입력을 데이터로 전달하고 escaping 테스트를 둔다.
7. 새 비밀번호 필드를 확대하지 않는다. 기존 password 호환성이 필요하면 비밀값을 argv·로그·session/build record에 남기지 않고 런타임에서만 사용한다.

## 6. 오류 UX

| 상황 | 기대 결과 |
|---|---|
| UTM 미설치·자동화 권한 거부 | 원인·복구 절차, Tart 작업은 계속 가능 |
| 불가 조합·ISO 없음·해시 불일치 | VM/세션 생성 전 실패 |
| OS 설치 미완료 | 설치·SSH 준비·build 재개 명령 안내 |
| SSH/guest agent 미준비 | 필요한 서비스를 구분하고 단계·timeout 표시 |
| 도구 검증 실패 | 명령·exit code·로그 표시, 이전 이미지 보존 |
| 외부에서 VM 삭제 | missing 반영, 관계없는 VM 생성/삭제 금지 |
| 프로필 변경 후 resume | 저장된 설정 사용 |
| isolated 불가 | 사전 오류, NAT는 명시 선택 |
| 긴 emulate 부팅 | 단계·경과 시간·로그 위치·timeout 설정 방법 |

## 7. 인수 기준

| ID | 기대 결과 | 검증 수준 |
|---|---|---|
| AC-01 | 기존 두 macOS 프로필과 과거 세션 사용 | 자동화 + 실VM 회귀 |
| AC-02 | Windows/Kali x86_64 게스트이고 hypervisor off | 구성 확인 + 실VM 시스템 정보 |
| AC-03 | Kali arm64 virtualize 부팅 | 구성 확인 + 실VM |
| AC-04 | ISO 생성→수동 설치→build→재사용 launch | 필수 세 게스트 실VM |
| AC-05 | 원본 UTM VM 복제 후 원본 보존 | 실VM |
| AC-06 | 사용자 스크립트 추가→재빌드→도구 검증 | Kali/Windows 실VM |
| AC-07 | OS별 명령·경로·파일 해시·작업 폴더 정상 | 자동화 + 실VM |
| AC-08 | 혼합 백엔드 세션과 설정 변경 후 생명주기 정상 | fake 통합 + 실VM |
| AC-09 | 실패 빌드의 이전 이미지 보존·부분 실패 복구 | fault injection |
| AC-10 | isolated 차단 또는 사전 거부, NAT 명시 선택 | 실VM 네트워크 |
| AC-11 | 외부 VM 삭제·인젝션·압축 탈출 방지 | 자동화 + 실VM 표본 |
| AC-12 | Nix build/fmt/lint/tests 통과, UTM 없이 비실VM 테스트 | 자동화 |

isolated가 불가능한 환경은 “NAT 지원, isolated 사전 거부”로 표시한다. 이를 isolated 지원 완료로 주장하지 않는다. 외부 조건으로 실제 게스트 검증을 못 하면 해당 AC를 unverified로 남긴다.

## 8. 구현 작업 순서

각 작업은 관련 실패 테스트 작성→실패 이유 확인→구현→대상 테스트 및 회귀 검사 순서로 진행한다. 새 파일명은 책임을 유지하는 범위에서 조정할 수 있으며 변경 이유를 기록한다.

### Task 1. UTM 자동화 계약과 호스트 검증

**추가:** `docs/utm-compatibility.md`, 이후 `crates/wargame-utm/tests/fixtures/`.

- [ ] UTM/utmctl 버전·help·AppleScript dictionary를 확인하고 지원 최소 버전을 기록한다.
- [ ] 앱 소유 임시 VM으로 ISO 연결, x86_64 설정, hypervisor off, UUID 조회, 복제, 시작/중지, 콘솔, 삭제를 확인한다.
- [ ] Windows 11 펌웨어/TPM/드라이버와 guest agent/SSH 주소 조회 경로를 검증한다.
- [ ] isolated/NAT와 호스트 연결의 지원 범위 및 불가 시 거부 경로를 기록한다.
- [ ] 비밀값 없는 명령/응답 fixture를 확보한다.

**산출 계약:** 재현 명령·버전·가능한 조합이 문서화되어 있다. 필수 OS 부팅을 막는 문제는 재현 증거와 해결안을 확보해 다음 작업에 반영한다.

### Task 2. 프로필·세션과 backend resolver

**수정:** core `domain/{profile,compat,session}.rs`, `config.rs`, `ports/{vm_provider,guest_transport}.rs`, `app/mod.rs`, `testing/*`, CLI `wiring.rs`.
**추가:** core `app/backends.rs`, `tests/backends.rs`.

- [ ] 과거 YAML/JSON을 그대로 읽는 테스트와 지원 조합 표 테스트를 작성한다.
- [ ] FR-1 스키마, endpoint, session snapshot과 resolver를 구현한다.
- [ ] 같은 이름의 Tart/UTM VM, 기본 설정 변경 후 resume, 서로 다른 user/port 세션 라우팅을 테스트한다.
- [ ] Tart/fake 구현을 갱신하고 core/CLI 회귀 테스트를 통과시킨다.

**산출 계약:** 신규 실행은 profile, 재개·삭제는 session으로 provider/connection을 선택한다. 호환성 검사는 부작용 전에 끝난다.

### Task 3. UTM provider와 doctor

**추가:** `crates/wargame-utm/Cargo.toml`, `src/{lib,provider,automation,config,console}.rs`, `tests/provider.rs`.
**수정:** workspace/CLI `Cargo.toml`, CLI `wiring.rs`, core `app/doctor.rs`/`error.rs`, 필요한 경우 `flake.nix`.

- [ ] fixture 기반 정상·잘못된 출력·권한 거부·timeout 테스트를 작성한다.
- [ ] create/clone/start/wait/status/stop/delete/list/console과 UUID 매핑을 구현한다.
- [ ] emulate hypervisor off와 virtualize 호환성 검사를 연결한다.
- [ ] prefix가 같아도 소유 record에 없는 VM 삭제가 거부되는지 검사한다.
- [ ] doctor 경로 탐색·버전·프로필별 의존성 검사를 검증한다.

**산출 계약:** UTM은 core VM port 구현체이며 외부 명령은 fake runner로 검증된다.

### Task 4. 설치 VM과 재사용 이미지 빌드

**추가:** core `app/profile_build.rs`, `domain/build.rs`, `ports/image_builder.rs`, `tests/profile_build.rs`; UTM `src/build.rs`.
**수정:** Tart `src/build.rs`, core exports/저장소, CLI `cli.rs`/`main.rs`/`wiring.rs`.

- [ ] create 재실행·소스 충돌·미설치 build·해시 오류·이전 이미지 보존 테스트를 작성한다.
- [ ] init/create/console과 소유 build record를 구현한다. 내장 템플릿은 Task 6에서 완성한다.
- [ ] ISO 및 UTM 원본 복제 경로를 연결하고 Tart build를 공통 계약에 맞춘다.
- [ ] 설치 대기·프로비저닝·검증·정상 종료·게시를 구분하고 해시를 저장한다.
- [ ] 재부팅·중단·디스크 부족·정리 실패를 주입해 복구를 검사한다.

**산출 계약:** build 성공은 검증된 정지 이미지 게시이며 실패·설치 대기를 ready로 표시하지 않는다.

### Task 5. OS별 연결과 bootstrap

**추가:** core `infra/powershell.rs`, `tests/guest_transport.rs`, `guest/bootstrap.ps1`.
**수정:** core `infra/ssh.rs`, `app/{launch,lifecycle,console}.rs`, `guest/bootstrap.sh`, CLI `profiles_builtin.rs`.

- [ ] Windows 인코딩, 공백/따옴표/한글 경로, 비기본 user/port 테스트를 작성한다.
- [ ] PowerShell 실행·Windows 전송·게스트 준비 검사·OS별 bootstrap을 구현한다.
- [ ] 작업 폴더·메타데이터·해시·도구 검증을 검사한다.
- [ ] 악성 압축 경로 및 링크 fixture로 디렉터리 탈출과 호스트 실행이 없음을 검증한다.

**산출 계약:** OS가 명령·경로 의미를 결정하고 프로비저닝 스크립트만 실행하며 문제 파일은 실행하지 않는다.

### Task 6. 프로필·사용자 도구·CLI/TUI

**추가:** `profiles/kali-arm64.yaml`, `profiles/kali-x86_64.yaml`, `profiles/windows-x86_64.yaml`, `profiles/scripts/kali/common.sh`, `profiles/scripts/windows/common.ps1`.
**수정:** CLI `profiles_builtin.rs`/`cli.rs`/`main.rs`/`output.rs`, `tui/{app,launch,sessions}.rs`, core `app/profiles.rs`, CLI `tests/cli.rs`, core `tests/{profiles,launch,lifecycle}.rs`.

- [ ] 내장 템플릿 파싱과 사용자 YAML/스크립트 보존 테스트를 추가한다.
- [ ] 기본 도구·verify 명령과 custom script 예시를 제공한다.
- [ ] profiles/doctor/launch/TUI에 backend/arch/mode/이미지 상태를 반영한다.
- [ ] 혼합 세션·과거 JSON으로 생명주기·gui·ssh를 검사한다.

**산출 계약:** §4 흐름이 완성되며 CLI/TUI는 같은 서비스와 상태를 사용한다.

### Task 7. 실VM 인수 검증과 문서

**추가:** `scripts/e2e-multi-os.sh`, `crates/wargame-utm/tests/e2e.rs`.
**수정:** `README.md`, `DESIGN.md`, `CHANGELOG.md`, `docs/verification.md`, `docs/next-steps.md`.

- [ ] 자동 검사와 기존 macOS 두 프로필 smoke를 실행한다.
- [ ] 필수 세 게스트에서 설치·build·launch·사용자 도구 재빌드를 수행한다.
- [ ] 중지→재개→콘솔→파일 확인→삭제 및 원본 보존을 검사한다.
- [ ] 네트워크 격리/거부, 실패 복구, emulate 설정을 검증한다.
- [ ] OS/ISO 해시, 호스트/UTM 버전, 명령·시간·로그와 AC별 결과를 기록한다.
- [ ] 최종 코드 검토로 요구 누락·회귀·소유권·인젝션 문제를 해결한다.

**산출 계약:** 문서와 실검증 결과가 일치하며 미검증 조합은 지원 완료로 표시하지 않는다.

## 9. 검사 명령과 완료 판정

```bash
nix develop -c cargo fmt --all --check
nix develop -c cargo clippy --workspace --all-targets -- -D warnings
nix develop -c cargo test --workspace
nix flake check
nix build
```

실VM 검사는 opt-in이다. 일반 cargo test에서 VM 생성·대용량 다운로드·앱 자동화를 수행하지 않는다. Linux에서도 비실VM 테스트가 실행 가능해야 한다.

기존 `scripts/e2e-smoke.sh`의 입력과 부작용을 확인한 뒤 준비된 macOS 두 프로필로 검사한다. 새 `e2e-multi-os.sh`는 프로필 이름을 받고 미리 빌드된 이미지에서 launch 이후를 검증한다. ISO 설치는 별도 수동 단계와 증거로 기록한다.

- [ ] AC-01–AC-12 각각 pass/fail/unverified를 기록했다.
- [ ] Windows/Kali x86_64 전체 VM 에뮬레이션 및 Kali arm64 virtualization을 실검증했다.
- [ ] Windows/Kali 사용자 도구 추가·재빌드가 동작한다.
- [ ] 기존 macOS와 과거 프로필/세션 호환성을 검증했다.
- [ ] mock 결과와 실VM 결과를 구분했고 미검증을 완료로 처리하지 않았다.
- [ ] 최종 변경 파일·검사 결과·실행 명령·남은 제한을 인계했다.

## 10. Claude 실행 지침 및 공식 근거

- 실행 시 저장소 지침과 git 상태를 다시 확인한다. 이 PRD 작성 시작 시 작업 트리는 깨끗했다.
- Task 1의 실제 외부 API 검증을 먼저 수행한다. 기존 코드를 재작성하지 말고 필요한 경계만 확장한다.
- 스크립트·의존성 추가 이유와 단계별 검증 증거를 기록한다. 비밀값·라이선스 키·ISO/VM 디스크는 저장소에 넣지 않는다.
- 과거 `.claude/workflows/mvp-build.js`의 작업 번호·모델·커밋 정책을 새 기능 요구로 해석하지 않는다. 커밋은 현재 실행 환경 지침을 따른다.
- 사용자에게는 구현 결과와 검증 여부를 구분해 보고한다. 이 문서 작성 후 구현 담당은 Claude이다.

공식 자료(구현 시 설치 버전과 대조):

- [Tart](https://tart.run/): Apple Silicon macOS/Linux VM 도구.
- [UTM scripting](https://docs.getutm.app/scripting/scripting/): 자동화와 앱 내부 utmctl 경로.
- [UTM scripting cheat sheet](https://docs.getutm.app/scripting/cheat-sheet/): 생성/복제, hypervisor 설정, 주소 조회, guest agent 예시.
- [UTM scripting reference](https://docs.getutm.app/scripting/reference/): backend/configuration/VM 조작 계약.

설계상 기대와 검증된 동작은 다르다. Windows 설치 조건, UTM 명령 옵션, 네트워크 격리와 콘솔 동작은 Task 1 및 실제 인수 검증으로 확정한다.

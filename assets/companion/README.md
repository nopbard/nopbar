# Companion v8 + v9 clips

오너가 승인한 `2026-11-12-companion-v8/preview/cut` 원본을 바이트 그대로 포함한다.
2026-11-13 흰 테·효과선 속 흰 조각 제거본. 그림의 재생성·재색칠·벡터화 금지.
2026-11-14 look_3·4·5는 오너 요청으로 고깔 안감(흰 띠)을 고깔 빨강으로 메우고 옛 챙 선을 지운 수정본(올려다볼 때 속이 비어 보이지 않게).

`frames.json`의 형식은 미리보기와 같다. 좌표는 자른 PNG의 픽셀 기준이다.
각 프레임은 `<name>_body.png`, `<name>_hat.png`, `<name>_full.png`를 갖는다.
2026-11-13 추가된 연속 프레임 64장을 사용한다. 원본 14장은 보관하지만
실제 클립에서 단일 원본은 상단바 L만 사용한다.

`clips.json`이 상태와 프레임을 연결한다. `frames`는 순서대로 재생할 이름,
`frame_ms`는 한 장의 표시 시간(양수 ms, 소수 허용), `repeat`는 반복 여부다.
`hold`는 프레임 인덱스별 시간 배수, `next`는 비반복 클립이 끝난 뒤의 클립,
`travel_ms`는 공중 이동 시간이다. notify의 1번은 3배, done의 1번은 1.4배,
peek의 1·2번은 4배로 머문다.
반복하지 않으면 마지막 그림을 유지하며 상태 전환 시간은 상태 모델이 관리한다.
`turn_left`·`turn_right`는 방향별 회전 순서다. 회전 프레임은 뒤집지 않는다.
`hide_launch` → `hide_air`는 숨기기, `show_air` → `land` → `peek`는 꺼내기다.
`slot`은 상단바 아이콘이다. 걷는 속도는 최신 미리보기의 52 DIP/s다.

예: 새 걷기 그림 8장을 넣으면 기존 `frames.json`에 메타데이터를 추가하고:

```json
"walk": {"frames":["walk_0","walk_1","walk_2","walk_3","walk_4","walk_5","walk_6","walk_7"],"frame_ms":100,"repeat":true}
```

Rust 코드는 수정하지 않는다. 빌드가 폴더의 PNG를 모두 자동 포함하므로 재빌드만 한다.
레이어와 메타데이터 누락, 잘못된 클립, 흰 테는 `cargo test -p dock --bin nopbar companion::clips`로 검사한다.
기준 edgecheck: alpha >120인 밝기 >190 픽셀의 3×3 이웃에 alpha <40이 있으면 실패.

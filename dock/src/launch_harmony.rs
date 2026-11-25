//! The "could not open" notice drawn off screen for the design audit
//! (tests only; see crate::popup::harmony): the rows `show` makes of a
//! notice, for a made-up pinned app whose file is gone.

use super::*;
use crate::popup::harmony::{nth_row, shoot, Shot};

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_launch_notice() {
    let _l = crate::i18n::test_lock("ko");
    let file = "Z:\\harmony\\memo.lnk";
    let target = Target { package_id: None, file: file.into(), verb: "open".into(), args: String::new(), dir: String::new(), name: "메모장".into(), source: Source::DockPin("메모장".into(), file.into()) };
    let n = notice(target, Reason::Missing, None);
    // As `show` lists it.
    let items = notice_rows(&n);
    shoot("launch-notice", &items, &Shot { hover: nth_row(&items, 1).or(nth_row(&items, 0)), ..Default::default() });
    // A long name and reason: the title wraps, the reason stops at three
    // lines.
    let long = Notice {
        title: "'분기 보고서 최종 수정본 (2026년 11월) 검토용.docx'을(를) 열지 못했어요".into(),
        detail: "이 종류의 파일을 열 앱이 없어요. 다른 앱을 고르거나 파일이 있는 폴더를 열어 확인해 보세요. 파일이 다른 PC에서 만들어졌다면 그 PC의 앱이 필요할 수도 있어요. 그래도 안 되면 파일이 손상됐을 수 있어요.".into(),
        ..n
    };
    let items = notice_rows(&long);
    shoot("launch-notice-long", &items, &Shot::default());
}

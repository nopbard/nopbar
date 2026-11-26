//! D2 cards use the settings theme, hits and UI Automation tree.
use super::{clip_to, model::Tab, App, Hit};
use crate::{
    companion::chat::{
        self,
        cli::{self, Status},
        state::Provider,
    },
    i18n::t,
};
use windows::{
    core::Result,
    Win32::Graphics::Direct2D::{Common::D2D_RECT_F, ID2D1DeviceContext},
};
pub const DETECT_TIMER: usize = 0xD215;
pub const SPINNER_TIMER: usize = 0xD217;

fn step_daily_limit(limit: u32, increase: bool) -> u32 {
    const STEP: u32 = 10;
    const MAX: u32 = 1000;
    match (limit, increase) {
        (0, false) => MAX,
        (0 | MAX.., true) => 0,
        (limit, true) => (limit / STEP + 1) * STEP,
        (limit, false) => (limit.saturating_sub(1) / STEP * STEP).clamp(STEP, MAX),
    }
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: l,
        top: t,
        right: r,
        bottom: b,
    }
}
pub fn connected(c: &chat::state::Config, account: bool) -> bool {
    c.connected(account) && (c.provider == Provider::ChatGpt || cli::usable(c.provider))
}
pub fn status_key(status: &Status) -> &'static str {
    match status {
        Status::Checking => "chat.status.checking",
        Status::Missing => "chat.status.missing",
        Status::Login => "chat.status.login",
        Status::Unknown => "chat.status.unknown",
        Status::Ready => "chat.status.ready",
        Status::Unsupported => "chat.status.update",
        Status::Failed => "chat.error.spawn",
    }
}
pub fn action_label(status: &Status) -> Option<&'static str> {
    match status {
        Status::Checking => None,
        Status::Missing => Some("chat.install"),
        Status::Unsupported => Some("chat.update"),
        Status::Login => Some("chat.login.cli"),
        Status::Failed => Some("chat.recheck"),
        Status::Ready | Status::Unknown => Some("chat.use"),
    }
}
#[derive(Debug, PartialEq)]
enum StepMark {
    Number,
    Running,
    Done,
}
fn step_mark(mode: chat::install::Mode, stage: &chat::install::Stage, index: usize) -> StepMark {
    use chat::install::{Mode, Stage};
    if matches!(stage, Stage::Done | Stage::Installed) {
        return StepMark::Done;
    }
    let active = match (mode, stage) {
        (_, Stage::Installing) => Some(0),
        (Mode::Install, Stage::Login) => Some(1),
        (Mode::Install, Stage::Verify) => Some(2),
        (Mode::Update, Stage::Verify) => Some(1),
        (Mode::Login, Stage::Login | Stage::Verify) => Some(0),
        _ => None,
    };
    match active {
        Some(i) if index < i => StepMark::Done,
        Some(i) if index == i => StepMark::Running,
        _ => StepMark::Number,
    }
}
impl App {
    pub(super) unsafe fn ai_button(
        &self,
        dc: &ID2D1DeviceContext,
        hits: &mut Vec<(D2D_RECT_F, Hit)>,
        r: D2D_RECT_F,
        label: &str,
        action: &'static str,
        clip: D2D_RECT_F,
    ) -> Result<()> {
        self.button(dc, r, label, self.hover == Some(Hit::Chat(action)))?;
        if let Some(r) = clip_to(r, clip) {
            hits.push((r, Hit::Chat(action)));
        }
        Ok(())
    }
    unsafe fn ai_navigation_row(
        &self,
        dc: &ID2D1DeviceContext,
        hits: &mut Vec<(D2D_RECT_F, Hit)>,
        r: D2D_RECT_F,
        action: &'static str,
        clip: D2D_RECT_F,
    ) -> Result<()> {
        self.fill_round(dc, self.wash(if self.hover == Some(Hit::Chat(action)) { 0.07 } else { 0.03 }), r, self.card_r())?;
        let cy = (r.top + r.bottom) / 2.;
        self.glyph(dc, crate::glyphs::Glyph::ChatBubble, 24., r.left + 28., cy, self.ink())?;
        self.label(dc, t(action), r.left + 52., self.text_top(cy, 14.), 14., true, self.ink())?;
        self.glyph(dc, crate::glyphs::Glyph::ChevronRight, 16., r.right - 24., cy, self.dim())?;
        if let Some(r) = clip_to(r, clip) { hits.push((r, Hit::Chat(action))); }
        Ok(())
    }

    pub(super) unsafe fn brand(
        &self,
        dc: &ID2D1DeviceContext,
        name: &str,
        x: f32,
        y: f32,
        size: f32,
    ) -> Result<()> {
        self.fill_round(
            dc,
            self.wash(0.10),
            rect(x, y, x + size, y + size),
            size / 2.,
        )?;
        let inset = size * 0.14;
        if self
            .provider_logo(dc, name, x + inset, y + inset, size - 2. * inset)
            .is_ok()
        {
            return Ok(());
        }
        // Missing official assets use initials, never a traced substitute.
        let label = if name == "Codex" { ">_" } else { &name[..1] };
        let w = self.text_w(label, size * 0.65, true);
        self.label(
            dc,
            label,
            x + (size - w) / 2.,
            y + (size - size * 0.85) / 2.,
            size * 0.65,
            true,
            self.ink(),
        )
        .map(|_| ())
    }
    pub(super) unsafe fn provider_logo(
        &self,
        dc: &ID2D1DeviceContext,
        name: &str,
        x: f32,
        y: f32,
        size: f32,
    ) -> Result<()> {
        crate::glyphs::provider_logo(dc, name, x, y, size, self.ink())
    }
    pub(super) unsafe fn paint_companion_ai(
        &self,
        dc: &ID2D1DeviceContext,
        hits: &mut Vec<(D2D_RECT_F, Hit)>,
        mut y: f32,
        left: f32,
        right: f32,
        clip: D2D_RECT_F,
    ) -> Result<f32> {
        #[cfg(not(test))]
        if self.tab() == Tab::CompanionAi
            && [Provider::ClaudeCode, Provider::Antigravity]
                .iter()
                .any(|p| cli::cached(*p).status == Status::Checking)
        {
            windows::Win32::UI::WindowsAndMessaging::SetTimer(
                Some(self.hwnd),
                SPINNER_TIMER,
                100,
                None,
            );
        }
        let start = y;
        let linked = self.cfg.chat.connected(chat::auth::connected());
        if self.tab() == Tab::Companion {
            self.fill_round(dc, self.wash(0.05), rect(left, y, left + 56., y + 56.), 14.)?;
            let pose = crate::companion::motion::Motion::new(0.).pose();
            let frame = &crate::companion::clips::catalog().frames[&pose.frame];
            crate::companion::render::Painter::new().paint(
                dc,
                &pose,
                left + 28.,
                y + 54.,
                50. / frame.h as f32,
                false,
                0.,
                self.scale,
            )?;
            self.label(
                dc,
                t("companion.title"),
                left + 72.,
                y + 16.,
                20.,
                true,
                self.ink(),
            )?;
            let enabled = self
                .rows()
                .iter()
                .position(|r| r.key == "companion.enabled")
                .unwrap();
            let r = rect(right - 42., y + 14., right, y + 42.);
            self.toggle_switch(
                dc,
                super::rows::Mirror {
                    on: false,
                    left,
                    right,
                },
                right,
                y + 28.,
                self.cfg.companion.enabled,
                self.hover == Some(Hit::Toggle(enabled)),
                "companion.enabled",
            )?;
            if let Some(r) = clip_to(r, clip) {
                hits.push((r, Hit::Toggle(enabled)));
            }
            y += 76.;
            let width = (right - left - 20.) / 3.;
            // What it does is the label; the gesture is the small line under it.
            let talk = connected(&self.cfg.chat, chat::auth::connected());
            for (i, (title, gesture, glyph)) in [
                (
                    if talk { "companion.talk" } else { "companion.react" },
                    "companion.click",
                    if talk {
                        crate::glyphs::Glyph::ChatBubble
                    } else {
                        crate::glyphs::Glyph::FingerTap
                    },
                ),
                ("companion.menu", "companion.right_click", crate::glyphs::Glyph::FanMenu),
                ("companion.move", "companion.drag", crate::glyphs::Glyph::MoveArrows),
            ]
            .into_iter()
            .enumerate()
            {
                let x = left + i as f32 * (width + 10.);
                self.fill_round(
                    dc,
                    self.wash(0.04),
                    rect(x, y, x + width, y + 60.),
                    self.card_r(),
                )?;
                self.glyph(dc, glyph, 20., x + 24., y + 30., self.ink())?;
                self.label(
                    dc,
                    t(title),
                    x + 44.,
                    self.text_top(y + 22., 13.),
                    13.,
                    true,
                    self.ink(),
                )?;
                self.label(
                    dc,
                    t(gesture),
                    x + 44.,
                    self.text_top(y + 40., 12.),
                    12.,
                    false,
                    self.dim(),
                )?;
            }
            self.note(
                super::layout::Kind::Picture,
                "companion.preview",
                rect(left, start, right, y + 72.),
            );
            self.flow.set(super::layout::Block::Card);
            return Ok(y + 72.);
        }
        self.ai_navigation_row(dc, hits, rect(left, y, right, y + 52.), "chat.history", clip)?;
        y += 60.;
        let keep = self.rows().iter().position(|r| r.key == "chat.keep_history").unwrap();
        self.label(dc, t("chat.keep_history"), left + 16., y + 12., 14., false, self.ink())?;
        self.toggle_switch_neutral(dc, super::rows::Mirror { on:false,left,right }, right - 16., y + 22., self.cfg.chat.keep_history, self.hover == Some(Hit::Toggle(keep)), "chat.keep_history")?;
        if let Some(r) = clip_to(rect(right - 58., y + 8., right - 16., y + 36.), clip) { hits.push((r, Hit::Toggle(keep))); }
        y += 56.;
        let p = if linked {
            self.cfg.chat.provider
        } else {
            Provider::ChatGpt
        };
        let detection = cli::cached(p);
        let controls = linked
            && (p == Provider::ChatGpt
                || matches!(detection.status, Status::Ready | Status::Unknown));
        let ix = left + 16.;
        let tx = ix + 24. + 12.;
        let edge = right - 16.;
        let header_h = 76.;
        let rows = if controls {
            if p == Provider::ChatGpt {
                3
            } else {
                2
            }
        } else {
            0
        };
        let h = header_h + if rows > 0 { 8. + rows as f32 * 42. } else { 0. };
        self.fill_round(
            dc,
            self.wash(0.05),
            rect(left, y, right, y + h),
            self.card_r(),
        )?;
        self.brand(dc, p.name(), ix, y + (header_h - 24.) / 2., 24.)?;
        let title = p.name();
        self.label(dc, title, tx, y + 16., 14., true, self.ink())?;
        let desc = t(if controls {
            "chat.ai.connected"
        } else if p == Provider::ChatGpt {
            "chat.status.login"
        } else {
            status_key(&detection.status)
        });
        let header_action = if !linked {
            Some("chat.login")
        } else if controls {
            Some("chat.disconnect")
        } else {
            action_label(&detection.status)
        };
        let bw = header_action
            .map(|k| (self.text_w(t(k), 13., false) + 24.).max(72.))
            .unwrap_or(24.);
        let desc = self.ellipsized(desc, 12., false, edge - bw - 12. - tx);
        self.label(dc, &desc, tx, y + 42., 12., false, self.dim())?;
        if let Some(label) = header_action {
            self.ai_button(
                dc,
                hits,
                rect(edge - bw, y + 24., edge, y + 52.),
                t(label),
                if !linked {
                    "chat.login"
                } else if controls {
                    "chat.disconnect"
                } else {
                    "chat.restore"
                },
                clip,
            )?;
        } else {
            self.ai_spinner(dc, edge - 10., y + header_h / 2.)?;
        }
        if controls {
            self.fill_round(
                dc,
                self.wash(0.09),
                rect(tx, y + header_h, edge, y + header_h + 1.),
                0.,
            )?;
            let mut cy = y + header_h + 25.;
            let model = if self.cfg.chat.active_model().is_empty() {
                t("chat.model.default")
            } else {
                self.cfg.chat.active_model()
            };
            self.label(
                dc,
                t("chat.model"),
                tx,
                self.text_top(cy, 13.),
                13.,
                false,
                self.ink(),
            )?;
            self.ai_menu(dc, hits, (edge, cy), model, "chat.model", clip)?;
            cy += 42.;
            if p == Provider::ChatGpt {
                self.label(
                    dc,
                    t("chat.account"),
                    tx,
                    self.text_top(cy, 13.),
                    13.,
                    false,
                    self.ink(),
                )?;
                self.ai_menu(
                    dc,
                    hits,
                    (edge, cy),
                    &chat::auth::label(),
                    "chat.account",
                    clip,
                )?;
                cy += 42.;
            }
            self.label(
                dc,
                t("chat.daily_limit"),
                tx,
                self.text_top(cy, 13.),
                13.,
                false,
                self.ink(),
            )?;
            let used = chat::transport::usage().today(chat::transport::local_day());
            let used = crate::i18n::tf("chat.ai.used", &[("n", &used.to_string())]);
            self.label(
                dc,
                &used,
                tx + self.text_w(t("chat.daily_limit"), 13., false) + 12.,
                self.text_top(cy, 12.),
                12.,
                false,
                self.dim(),
            )?;
            let r = rect(edge - 112., cy - 14., edge, cy + 14.);
            self.fill_round(
                dc,
                self.ctl_fill(crate::theme::State::Rest),
                r,
                self.ctl_r(),
            )?;
            let brush = dc.CreateSolidColorBrush(&self.wash(0.15), None)?;
            dc.DrawRoundedRectangle(
                &windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                    rect: r,
                    radiusX: self.ctl_r(),
                    radiusY: self.ctl_r(),
                },
                &brush,
                1.,
                None,
            );
            for (x, sign, action) in [
                (r.left, "−", "chat.limit.minus"),
                (r.right - 32., "+", "chat.limit.plus"),
            ] {
                let part = rect(x, r.top, x + 32., r.bottom);
                self.label(
                    dc,
                    sign,
                    x + (32. - self.text_w(sign, 13., false)) / 2.,
                    self.text_top(cy, 13.),
                    13.,
                    false,
                    self.ink(),
                )?;
                if let Some(r) = clip_to(part, clip) {
                    hits.push((r, Hit::Chat(action)));
                }
            }
            let value = if self.cfg.chat.daily_limit == 0 {
                "∞".into()
            } else {
                self.cfg.chat.daily_limit.to_string()
            };
            self.label(
                dc,
                &value,
                (r.left + r.right - self.text_w(&value, 13., false)) / 2.,
                self.text_top(cy, 13.),
                13.,
                false,
                self.ink(),
            )?;
        }
        y += h + 24.;
        self.label(
            dc,
            t(if linked {
                "chat.ai.switch"
            } else {
                "chat.ai.existing"
            }),
            left,
            y,
            12.,
            true,
            self.dim(),
        )?;
        y += 24.;
        for provider in [
            Provider::ChatGpt,
            Provider::ClaudeCode,
            Provider::Antigravity,
        ] {
            if provider == p {
                continue;
            }
            self.fill_round(
                dc,
                self.wash(0.03),
                rect(left, y, right, y + 68.),
                self.card_r(),
            )?;
            self.brand(dc, provider.name(), ix, y + 22., 24.)?;
            self.label(dc, provider.name(), tx, y + 12., 14., true, self.ink())?;
            let d = cli::cached(provider);
            let status = if provider == Provider::ChatGpt {
                if chat::auth::connected() {
                    "chat.ai.connected"
                } else {
                    "chat.status.login"
                }
            } else {
                status_key(&d.status)
            };
            let label = if provider == Provider::ChatGpt {
                Some(if chat::auth::connected() {
                    "chat.use"
                } else {
                    "chat.login.cli"
                })
            } else {
                action_label(&d.status)
            };
            let bw = label
                .map(|k| (self.text_w(t(k), 13., false) + 24.).max(72.))
                .unwrap_or(24.);
            let subtitle = self.ellipsized(t(status), 12., false, edge - bw - 12. - tx);
            self.label(dc, &subtitle, tx, y + 36., 12., false, self.dim())?;
            if let Some(label) = label {
                let action = match provider {
                    Provider::ChatGpt => "chat.use.chatgpt",
                    Provider::ClaudeCode => "chat.use.claude",
                    Provider::Antigravity => "chat.use.agy",
                };
                self.ai_button(
                    dc,
                    hits,
                    rect(edge - bw, y + 20., edge, y + 48.),
                    t(label),
                    action,
                    clip,
                )?;
            } else {
                self.ai_spinner(dc, edge - 10., y + 34.)?;
            }
            y += 72.;
        }
        y += 16.;
        self.ai_navigation_row(dc, hits, rect(left, y, right, y + 52.), "chat.ai.agents", clip)?;
        y += 52.;
        self.note(
            super::layout::Kind::Picture,
            "chat.ai.cards",
            rect(left, start, right, y),
        );
        self.flow.set(super::layout::Block::Card);
        Ok(y)
    }
    pub(super) unsafe fn ai_menu(
        &self,
        dc: &ID2D1DeviceContext,
        hits: &mut Vec<(D2D_RECT_F, Hit)>,
        (right, cy): (f32, f32),
        value: &str,
        action: &'static str,
        clip: D2D_RECT_F,
    ) -> Result<()> {
        let r = rect(right - super::rows::tok::FIELD_W, cy - 14., right, cy + 14.);
        self.menu_value(dc, r, value, self.state_of(Hit::Chat(action)), false)?;
        if let Some(r) = clip_to(r, clip) {
            hits.push((r, Hit::Chat(action)));
        }
        Ok(())
    }
    unsafe fn ai_spinner(&self, dc: &ID2D1DeviceContext, x: f32, y: f32) -> Result<()> {
        use windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE;
        let phase = if cfg!(test) {
            0
        } else {
            (unsafe { windows::Win32::System::SystemInformation::GetTickCount64() } / 100 % 8)
                as usize
        };
        for i in 0..8 {
            let angle = i as f32 * std::f32::consts::TAU / 8.;
            let mut color = self.dim();
            color.a *= 0.2 + 0.8 * ((i + 8 - phase) % 8) as f32 / 7.;
            let brush = dc.CreateSolidColorBrush(&color, None)?;
            dc.FillEllipse(
                &D2D1_ELLIPSE {
                    point: windows_numerics::Vector2 {
                        X: x + angle.cos() * 6.,
                        Y: y + angle.sin() * 6.,
                    },
                    radiusX: 1.4,
                    radiusY: 1.4,
                },
                &brush,
            );
        }
        Ok(())
    }
    pub(super) fn chat_action(&mut self, action: &str) {
        match action {
            "chat.history" => { std::thread::spawn(|| chat::hint::notify_kind("CompanionHistory")); }
            "chat.restore" => {
                if cli::cached(self.cfg.chat.provider).status == Status::Failed {
                    cli::refresh(self.hwnd);
                } else {
                    chat::install::open(self.cfg.chat.provider);
                }
                self.focus = None;
            }
            "chat.install.confirm" => {
                if let Some(d) =
                    chat::install::snapshot().filter(|d| d.stage == chat::install::Stage::Installed)
                {
                    self.cfg.chat.activate(d.provider);
                    chat::install::close();
                    self.chat_changed();
                }
            }
            "chat.install.cancel" | "chat.install.done" => chat::install::close(),
            "chat.install.details" => chat::install::details(),
            "chat.install.begin" => chat::install::begin(self.hwnd),
            "chat.install.help" => {
                if let Some(d) = chat::install::snapshot() {
                    crate::launch::open(&crate::launch::Target::new(
                        chat::install::source(d.provider),
                        "",
                        crate::launch::Source::Settings,
                    ));
                }
            }
            "chat.ai.agents" => {
                if let Some(i) = self.sections.iter().position(|s| s.tab == Tab::Agents) {
                    self.go_to_section(i, super::Nav::Open);
                }
            }
            "chat.limit.minus" | "chat.limit.plus" => {
                let current = self.cfg.chat.daily_limit;
                let next = step_daily_limit(current, action == "chat.limit.plus");
                if next != current {
                    self.cfg.chat.daily_limit = next;
                    self.touch("chat.daily_limit");
                    self.send_preview();
                }
            }
            "chat.disconnect" => {
                if self.cfg.chat.provider == Provider::ChatGpt {
                    chat::settings::begin(self.hwnd, action)
                } else {
                    self.cfg.chat.disconnect_cli();
                    self.chat_changed();
                }
            }
            "chat.model" => {
                let choice = if self.cfg.chat.provider == Provider::ChatGpt {
                    chat::settings::pick_model(self.hwnd)
                } else {
                    let models = cli::models(self.cfg.chat.provider);
                    let labels = models
                        .iter()
                        .map(|(_, label)| label.clone())
                        .collect::<Vec<_>>();
                    chat::settings::pick(self.hwnd, &labels).map(|i| models[i].0.clone())
                };
                if let Some(model) = choice {
                    self.cfg.chat.set_model(model);
                    self.touch("chat.model");
                    self.send_preview();
                }
            }
            "chat.account" => {
                let mut labels = chat::session::snapshot().accounts;
                let n = labels.len();
                labels.push(t("chat.add").into());
                if let Some(i) = chat::settings::pick(self.hwnd, &labels) {
                    if i == n {
                        chat::settings::begin(self.hwnd, "chat.add")
                    } else {
                        chat::settings::select_account(self.hwnd, i)
                    }
                }
            }
            "chat.use.chatgpt" => {
                self.cfg.chat.activate(Provider::ChatGpt);
                self.chat_changed();
                if chat::auth::connected() {
                    chat::settings::begin(self.hwnd, "chat.models")
                } else {
                    chat::settings::begin(self.hwnd, "chat.login")
                }
            }
            "chat.use.claude" | "chat.use.agy" => {
                let p = if action == "chat.use.claude" {
                    Provider::ClaudeCode
                } else {
                    Provider::Antigravity
                };
                let d = cli::cached(p);
                match d.status {
                    Status::Checking => {}
                    Status::Missing | Status::Login | Status::Unsupported => {
                        self.focus = None;
                        chat::install::open(p);
                    }
                    Status::Failed => cli::refresh(self.hwnd),
                    _ => {
                        self.cfg.chat.activate(p);
                        self.chat_changed();
                        if d.status == Status::Ready {
                            chat::auth::cli_first_connection();
                        }
                    }
                }
            }
            "chat.login" => {
                self.cfg.chat.activate(Provider::ChatGpt);
                self.chat_changed();
                chat::settings::begin(self.hwnd, action);
            }
            _ => {}
        }
        self.redraw();
    }
    fn chat_changed(&mut self) {
        self.touch("chat.provider");
        self.send_preview();
    }
    pub(super) fn install_updated(&mut self) {
        if let Some(d) = chat::install::snapshot() {
            if d.stage == chat::install::Stage::Done {
                if d.mode != chat::install::Mode::Update {
                    self.cfg.chat.activate(d.provider);
                    chat::auth::cli_first_connection();
                }
                chat::install::close();
                self.chat_changed();
            }
        }
    }
    pub(super) unsafe fn paint_install(
        &self,
        dc: &ID2D1DeviceContext,
        hits: &mut Vec<(D2D_RECT_F, Hit)>,
    ) -> Result<()> {
        use chat::install::{self, Mode, Stage};
        use windows::Win32::Graphics::{
            Direct2D::{D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_NONE},
            DirectWrite::{
                DWRITE_FONT_WEIGHT_NORMAL, DWRITE_LINE_SPACING_METHOD_UNIFORM, DWRITE_TEXT_METRICS,
                DWRITE_WORD_WRAPPING_WRAP,
            },
        };
        use windows_numerics::Vector2;
        let Some(d) = install::snapshot() else {
            return Ok(());
        };
        #[cfg(not(test))]
        if install::progressing() {
            windows::Win32::UI::WindowsAndMessaging::SetTimer(
                Some(self.hwnd),
                SPINNER_TIMER,
                100,
                None,
            );
        }
        hits.clear();
        let screen = rect(0., 0., super::win_w(), super::win_h());
        self.fill_round(dc, super::col(0., 0., 0., 0.6), screen, 0.)?;
        let width = 480.0_f32.min(super::win_w() - 40.);
        let left = (super::win_w() - width) / 2.;
        let right = left + width;
        let x = left + 24.;
        let edge = right - 24.;
        let w = edge - x;
        let title = crate::i18n::tf(
            match d.mode {
                Mode::Install => "chat.install.title",
                Mode::Update => "chat.update.title",
                Mode::Login => "chat.login.title",
            },
            &[("name", d.provider.name())],
        );
        let title_h = self.para_h(&title, 16., w - 76.).max(28.);
        let steps: &[&str] = match d.mode {
            Mode::Install => &[
                "chat.install.step1",
                "chat.install.step2",
                "chat.install.step3",
            ],
            Mode::Update => &["chat.update", "chat.install.step3"],
            Mode::Login => &["chat.install.step2"],
        };
        let status = match d.stage {
            Stage::Failed(k) => k,
            Stage::Cancelled => "chat.install.cancelled",
            Stage::Installed => "chat.install.installed",
            _ => "",
        };
        let status_h = if status.is_empty() {
            0.
        } else {
            self.para_h(t(status), 12., w) + 12.
        };
        let command = install::command_text(d.provider, d.mode);
        let (code, _, _) = self.gfx.text_layout(
            &crate::gfx::keep_words(&command),
            "Consolas",
            11.,
            DWRITE_FONT_WEIGHT_NORMAL,
        )?;
        let line_h = 16. * crate::fonts::scale();
        code.SetMaxWidth(w - 28.)?;
        code.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
        code.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, line_h, line_h * 0.8)?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        code.GetMetrics(&mut metrics)?;
        let code_view_h = metrics.height.min(3. * line_h);
        let max_scroll = (metrics.height - code_view_h).max(0.);
        install::scroll_limit(d.generation, max_scroll);
        let details_h = if d.details {
            code_view_h + 16. + 12.
        } else {
            0.
        };
        let height =
            24. + title_h + 20. + steps.len() as f32 * 28. + status_h + details_h + 20. + 32. + 24.;
        let top = (super::win_h() - height) / 2.;
        let bg = if self.light {
            super::col(0.96, 0.96, 0.97, 1.)
        } else {
            super::col(0.137, 0.137, 0.145, 1.)
        };
        self.fill_round(dc, bg, rect(left, top, right, top + height), 16.)?;
        let clip = rect(left + 8., top + 8., right - 8., top + height - 8.);
        dc.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let mut y = top + 24.;
        self.brand(dc, d.provider.name(), x, y, 28.)?;
        self.para(dc, &title, x + 40., y + 2., 16., true, w - 76., self.ink())?;
        let help = rect(edge - 28., y, edge, y + 28.);
        self.glyph(
            dc,
            crate::glyphs::Glyph::External,
            14.,
            edge - 14.,
            y + 14.,
            if self.hover == Some(Hit::Chat("chat.install.help")) {
                self.ink()
            } else {
                self.dim()
            },
        )?;
        hits.push((help, Hit::Chat("chat.install.help")));
        y += title_h + 20.;
        for (index, title) in steps.iter().enumerate() {
            match step_mark(d.mode, &d.stage, index) {
                StepMark::Running => self.ai_spinner(dc, x + 8., y + 10.)?,
                StepMark::Done => self.glyph(
                    dc,
                    crate::glyphs::Glyph::Check,
                    16.,
                    x + 8.,
                    y + 10.,
                    self.ink(),
                )?,
                StepMark::Number => {
                    self.label(
                        dc,
                        &(index + 1).to_string(),
                        x + 4.,
                        y,
                        13.,
                        true,
                        self.dim(),
                    )?;
                }
            }
            self.label(dc, t(title), x + 28., y, 13., false, self.ink())?;
            y += 28.;
        }
        if !status.is_empty() {
            self.para(dc, t(status), x, y, 12., false, w, self.dim())?;
            y += status_h;
        }
        if d.details {
            let box_h = code_view_h + 16.;
            self.fill_round(dc, self.wash(0.07), rect(x, y, edge, y + box_h), 6.)?;
            let scroll = d.scroll.min(max_scroll);
            dc.PushAxisAlignedClip(
                &rect(x + 8., y + 8., edge - 12., y + 8. + code_view_h),
                D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            );
            let ink = dc.CreateSolidColorBrush(&self.dim(), None)?;
            dc.DrawTextLayout(
                Vector2 {
                    X: x + 8.,
                    Y: y + 8. - scroll,
                },
                &code,
                &ink,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
            dc.PopAxisAlignedClip();
            if max_scroll > 0. {
                let thumb_h = (code_view_h * code_view_h / metrics.height).max(8.);
                let thumb_y = y + 8. + (code_view_h - thumb_h) * scroll / max_scroll;
                self.fill_round(
                    dc,
                    self.dim(),
                    rect(edge - 6., thumb_y, edge - 3., thumb_y + thumb_h),
                    1.5,
                )?;
            }
            y += details_h;
        }
        y += 20.;
        let link = t("chat.install.details");
        let link_w = self.text_w(link, 12., false);
        self.label(
            dc,
            link,
            x,
            self.text_top(y + 16., 12.),
            12.,
            false,
            if self.hover == Some(Hit::Chat("chat.install.details")) {
                self.ink()
            } else {
                self.dim()
            },
        )?;
        hits.push((
            rect(x, y, x + link_w + 8., y + 32.),
            Hit::Chat("chat.install.details"),
        ));
        let primary = match d.stage {
            Stage::Installed => Some(("chat.install.confirm", "chat.install.confirm")),
            Stage::Done => Some(("chat.install.done", "chat.install.done")),
            Stage::Confirm | Stage::Cancelled => Some((
                match d.mode {
                    Mode::Install => "chat.install.begin",
                    Mode::Update => "chat.update",
                    Mode::Login => "chat.login.cli",
                },
                "chat.install.begin",
            )),
            Stage::Failed(_) => Some(("chat.install.retry", "chat.install.begin")),
            _ => None,
        };
        let mut button_right = edge;
        if let Some((label, action)) = primary {
            let bw = (self.text_w(t(label), 13., false) + 28.).max(80.);
            let r = rect(button_right - bw, y, button_right, y + 32.);
            self.button_emphasis(dc, r, t(label), self.hover == Some(Hit::Chat(action)), true)?;
            hits.push((r, Hit::Chat(action)));
            button_right -= bw + 8.;
        }
        if !matches!(d.stage, Stage::Installed | Stage::Done) {
            let bw = (self.text_w(t("chat.install.cancel"), 13., false) + 28.).max(80.);
            self.ai_button(
                dc,
                hits,
                rect(button_right - bw, y, button_right, y + 32.),
                t("chat.install.cancel"),
                "chat.install.cancel",
                clip,
            )?;
        }
        dc.PopAxisAlignedClip();
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn step_progress_tracks_install_login_and_verification() {
        use chat::install::{Mode, Stage};
        assert_eq!(
            step_mark(Mode::Install, &Stage::Confirm, 0),
            StepMark::Number
        );
        assert_eq!(
            step_mark(Mode::Install, &Stage::Installing, 0),
            StepMark::Running
        );
        assert_eq!(step_mark(Mode::Install, &Stage::Login, 0), StepMark::Done);
        assert_eq!(
            step_mark(Mode::Install, &Stage::Login, 1),
            StepMark::Running
        );
        assert_eq!(step_mark(Mode::Install, &Stage::Login, 2), StepMark::Number);
        assert_eq!(step_mark(Mode::Update, &Stage::Verify, 0), StepMark::Done);
        assert_eq!(
            step_mark(Mode::Update, &Stage::Verify, 1),
            StepMark::Running
        );
        assert_eq!(step_mark(Mode::Login, &Stage::Verify, 0), StepMark::Running);
        for index in 0..3 {
            assert_eq!(
                step_mark(Mode::Install, &Stage::Done, index),
                StepMark::Done
            );
        }
    }
    #[test]
    fn status_actions_are_single_and_specific() {
        for (status, action) in [
            (Status::Checking, None),
            (Status::Missing, Some("chat.install")),
            (Status::Unsupported, Some("chat.update")),
            (Status::Login, Some("chat.login.cli")),
            (Status::Ready, Some("chat.use")),
            (Status::Failed, Some("chat.recheck")),
        ] {
            assert_eq!(action_label(&status), action);
        }
    }
    #[test]
    fn switching_keeps_accounts_and_only_one_provider_active() {
        let mut c = chat::state::Config::default();
        assert!(!connected(&c, false));
        assert!(connected(&c, true));
        c.activate(Provider::ClaudeCode);
        assert!(connected(&c, false));
        c.set_model("sonnet".into());
        c.activate(Provider::Antigravity);
        assert_eq!(c.active_model(), "");
        assert!(c.claude_connected);
        c.disconnect_cli();
        assert!(!connected(&c, true));
        c.activate(Provider::ClaudeCode);
        assert_eq!(c.active_model(), "sonnet");
    }
}

#[cfg(test)]
mod daily_limit_tests {
    use super::step_daily_limit;

    #[test]
    fn stepper_orders_limits_from_ten_to_unlimited() {
        assert_eq!(step_daily_limit(10, false), 10);
        for limit in (10..1000).step_by(10) {
            let next = step_daily_limit(limit, true);
            assert_eq!(next, limit + 10);
            assert_eq!(step_daily_limit(next, false), limit);
        }
        assert_eq!(step_daily_limit(1000, true), 0);
        assert_eq!(step_daily_limit(0, true), 0);
        assert_eq!(step_daily_limit(0, false), 1000);
        assert_eq!(step_daily_limit(15, true), 20);
        assert_eq!(step_daily_limit(15, false), 10);
    }
}

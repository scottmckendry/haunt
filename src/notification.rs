//! Notification lifetime, animation, and rendering.

use std::time::{Duration, Instant};

use crate::{config::Config, paint::Area};

const ANIMATION: Duration = Duration::from_millis(240);
const LIFETIME: Duration = Duration::from_secs(5);

pub(crate) enum NotificationKind {
    Help,
    Success(String),
    Error(String),
}

pub(crate) struct Notification {
    kind: NotificationKind,
    created: Instant,
}

impl Notification {
    pub(crate) fn new(kind: NotificationKind, now: Instant) -> Self {
        Self { kind, created: now }
    }

    pub(crate) fn expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.created) >= LIFETIME
    }

    fn slide_offset(&self, now: Instant, width: u16) -> usize {
        let elapsed = now.saturating_duration_since(self.created);
        let remaining = LIFETIME.saturating_sub(elapsed);
        let visible = elapsed.min(remaining).min(ANIMATION).as_millis();
        (width as usize * (ANIMATION.as_millis() - visible) as usize)
            / ANIMATION.as_millis() as usize
    }

    pub(crate) fn animating(&self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.created);
        elapsed < ANIMATION || elapsed >= LIFETIME - ANIMATION
    }
}

pub(crate) fn paint_notifications(
    out: &mut Vec<u8>,
    area: Area,
    notifications: &[Notification],
    cfg: &Config,
    now: Instant,
) {
    let mut y = area.y;
    for notification in notifications.iter().rev() {
        let used = y.saturating_sub(area.y);
        if used >= area.h {
            break;
        }
        let available = Area {
            x: area.x,
            y,
            w: area.w,
            h: area.h - used,
        };
        let offset = notification.slide_offset(now, area.w);
        let height = match &notification.kind {
            NotificationKind::Help => {
                paint_overlay_offset(out, available, &help_lines(cfg), "\x1b[36m", "", offset)
            }
            NotificationKind::Success(message) => {
                paint_message(out, available, message, false, offset)
            }
            NotificationKind::Error(message) => {
                paint_message(out, available, message, true, offset)
            }
        };
        y = y.saturating_add(height as u16);
    }
}

struct OverlayLine {
    text: String,
    leader: Option<(usize, usize)>,
}

fn help_lines(cfg: &Config) -> Vec<OverlayLine> {
    let mut entries = Vec::new();
    if let Some(bind) = cfg.help_bind {
        entries.push((bind.to_string(), "help".to_string()));
    }
    entries.push((cfg.reload_bind.to_string(), "reload".to_string()));
    entries.extend(cfg.apps.iter().map(|app| {
        (
            app.chord.to_string(),
            app.name
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>(),
        )
    }));
    let cell_width = |text: &str| -> usize {
        text.chars()
            .map(|c| {
                let c = if c.is_control() { ' ' } else { c };
                libghostty_vt::unicode::grapheme_width(&[c]).1 as usize
            })
            .sum()
    };
    let width = entries
        .iter()
        .map(|(bind, _)| cell_width(bind))
        .max()
        .unwrap_or(0);
    let name_width = entries
        .iter()
        .map(|(_, name)| cell_width(name))
        .max()
        .unwrap_or(0);
    // A minimum content width keeps short lists from becoming tall and narrow.
    let total_width = 32.max(width + name_width + 4);
    entries
        .iter()
        .map(|(bind, name)| {
            let start = cell_width(bind) + 1;
            let dots = total_width - cell_width(bind) - cell_width(name) - 2;
            OverlayLine {
                text: format!("{} {} {}", bind, ".".repeat(dots), name),
                leader: Some((start, start + dots)),
            }
        })
        .collect()
}

#[cfg(test)]
fn paint_notification(out: &mut Vec<u8>, area: Area, notification: &Notification) {
    match &notification.kind {
        NotificationKind::Success(message) => {
            paint_message(out, area, message, false, 0);
        }
        NotificationKind::Error(message) => {
            paint_message(out, area, message, true, 0);
        }
        NotificationKind::Help => panic!("expected a message"),
    }
}

fn paint_message(
    out: &mut Vec<u8>,
    area: Area,
    message: &str,
    is_error: bool,
    offset: usize,
) -> usize {
    let style = if is_error { "\x1b[31m" } else { "\x1b[32m" };
    let icon = if is_error { "✗ " } else { "✓ " };
    let message = if is_error { message.trim() } else { message };
    let lines: Vec<_> = message
        .split('\n')
        .map(|text| OverlayLine {
            text: text.into(),
            leader: None,
        })
        .collect();
    paint_overlay_offset(out, area, &lines, style, icon, offset)
}

#[cfg(test)]
fn paint_overlay(
    out: &mut Vec<u8>,
    area: Area,
    content: &[OverlayLine],
    text_style: &str,
    icon: &str,
) {
    paint_overlay_offset(out, area, content, text_style, icon, 0);
}

fn paint_overlay_offset(
    out: &mut Vec<u8>,
    area: Area,
    content: &[OverlayLine],
    text_style: &str,
    icon: &str,
    offset: usize,
) -> usize {
    if area.w == 0 || area.h == 0 {
        return 0;
    }
    // Keep horizontal padding, but place the top and bottom borders against the text.
    let bordered = area.w >= 5 && area.h >= 3;
    let inset = if bordered { 2 } else { 0 };
    let vertical = usize::from(bordered);
    let content_width = area.w as usize - inset * 2;
    let content_height = area.h as usize - vertical * 2;
    let mut lines = vec![(String::new(), 0usize)];
    let mut clipped = false;
    let prefix = if content_width >= 8 { icon } else { "" };
    let mut logical_column = 0;
    let mut logical_row = 0;
    // Wrap by terminal cells. Newlines are layout, never terminal control bytes.
    let chars = content
        .iter()
        .enumerate()
        .flat_map(|(i, line)| (i > 0).then_some('\n').into_iter().chain(line.text.chars()));
    for c in prefix.chars().chain(chars) {
        if c == '\n' {
            if lines.len() == content_height {
                clipped = true;
                break;
            }
            lines.push((String::new(), 0));
            logical_column = 0;
            logical_row += 1;
            continue;
        }
        let c = if c.is_control() { ' ' } else { c };
        let w = libghostty_vt::unicode::grapheme_width(&[c]).1 as usize;
        if w == 0 {
            continue;
        }
        if w > content_width {
            logical_column += w;
            continue;
        }
        let columns = content.get(logical_row).and_then(|line| line.leader);
        if lines.last().unwrap().1 + w > content_width {
            if lines.len() == content_height {
                clipped = true;
                break;
            }
            let style = match columns {
                Some((_, end)) if logical_column >= end => "\x1b[0m",
                Some((start, _)) if logical_column >= start => "\x1b[90m",
                _ => text_style,
            };
            lines.push((style.to_string(), 0));
        }
        let line = lines.last_mut().unwrap();
        if let Some((start, end)) = columns {
            if logical_column == start {
                line.0.push_str("\x1b[90m");
            } else if logical_column == end {
                line.0.push_str("\x1b[0m");
            }
        }
        line.0.push(c);
        line.1 += w;
        logical_column += w;
    }
    if clipped {
        *lines.last_mut().unwrap() = ("…".into(), 1);
    }
    let mut width = lines.iter().map(|line| line.1).max().unwrap_or(0) + inset * 2;
    let title = bordered && area.w >= 11;
    if title {
        width = width.max(11);
    }
    if width == 0 {
        return 0;
    }
    let height = lines.len() + vertical * 2;
    let offset = offset * width / area.w as usize;
    let x = area.x as usize + area.w as usize - width + 1 + offset;
    if offset >= width {
        return height;
    }
    out.extend_from_slice(b"\x1b7");
    for row in 0..height {
        let mut row_out = Vec::new();
        {
            let out = &mut row_out;
            if bordered && (row == 0 || row == lines.len() + 1) {
                out.extend_from_slice(if row == 0 { "╭" } else { "╰" }.as_bytes());
                let title_width = if row == 0 && title {
                    out.extend_from_slice("─ \x1b[1;36mhaunt\x1b[22;90m ".as_bytes());
                    8
                } else {
                    0
                };
                for _ in 0..width - 2 - title_width {
                    out.extend_from_slice("─".as_bytes());
                }
                out.extend_from_slice(if row == 0 { "╮" } else { "╯" }.as_bytes());
            } else {
                let (text, cells) = &lines[row - vertical];
                if bordered {
                    out.extend_from_slice("│ ".as_bytes());
                }
                out.extend_from_slice(text_style.as_bytes());
                out.extend_from_slice(text.as_bytes());
                out.extend_from_slice(b"\x1b[90m");
                out.extend(std::iter::repeat_n(
                    b' ',
                    width - inset - cells - usize::from(bordered),
                ));
                if bordered {
                    out.extend_from_slice("│".as_bytes());
                }
            }
        }
        let clipped = clip_row(&row_out, width - offset);
        out_extend_row(out, &clipped, area.y as usize + row + 1, x);
    }
    out.extend_from_slice(b"\x1b[0m\x1b8");
    height
}

fn out_extend_row(out: &mut Vec<u8>, row: &[u8], y: usize, x: usize) {
    out.extend_from_slice(format!("\x1b[{y};{x}H\x1b[0;90m").as_bytes());
    out.extend_from_slice(row);
}

// Preserve SGR sequences while clipping a rendered row by terminal cells.
fn clip_row(row: &[u8], max: usize) -> Vec<u8> {
    let text = std::str::from_utf8(row).expect("overlay is UTF-8");
    let mut chars = text.chars().peekable();
    let mut result = String::new();
    let mut cells = 0;
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            result.push(c);
            for next in chars.by_ref() {
                result.push(next);
                if next == 'm' {
                    break;
                }
            }
        } else {
            let width = libghostty_vt::unicode::grapheme_width(&[c]).1 as usize;
            if cells + width > max {
                break;
            }
            result.push(c);
            cells += width;
        }
    }
    result.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_lifetime_and_animation_boundaries() {
        let now = Instant::now();
        let notification = Notification::new(NotificationKind::Help, now);
        assert!(notification.animating(now));
        assert!(!notification.animating(now + ANIMATION));
        assert!(notification.animating(now + LIFETIME - ANIMATION));
        assert!(!notification.expired(now + LIFETIME - Duration::from_millis(1)));
        assert!(notification.expired(now + LIFETIME));
    }

    #[test]
    fn notifications_stack_and_slide_without_writing_past_the_edge() {
        let now = Instant::now();
        let notifications = vec![
            Notification::new(NotificationKind::Success("older".into()), now - ANIMATION),
            Notification::new(NotificationKind::Success("newer".into()), now - ANIMATION),
        ];
        let area = Area {
            x: 0,
            y: 0,
            w: 30,
            h: 10,
        };
        let mut out = Vec::new();
        paint_notifications(
            &mut out,
            area,
            &notifications,
            &Config::parse("").unwrap(),
            now,
        );
        let text = String::from_utf8(out).unwrap();
        assert!(text.find("newer").unwrap() < text.find("older").unwrap());
        assert!(text.contains("\x1b[4;"));
        assert_eq!(notifications[0].slide_offset(now, 30), 0);
        assert_eq!(
            notifications[0].slide_offset(now - ANIMATION + LIFETIME, 30),
            30
        );
        assert_eq!(clip_row("\x1b[31m界x\x1b[0m".as_bytes(), 1), b"\x1b[31m");
    }

    #[test]
    fn help_uses_the_notification_stack() {
        let now = Instant::now();
        let notifications = vec![Notification::new(NotificationKind::Help, now - ANIMATION)];
        let mut out = Vec::new();
        paint_notifications(
            &mut out,
            Area {
                x: 0,
                y: 0,
                w: 40,
                h: 10,
            },
            &notifications,
            &Config::parse("").unwrap(),
            now,
        );
        assert!(String::from_utf8(out).unwrap().contains("help"));
    }

    #[test]
    fn help_overlay_preserves_styles_when_wrapped() {
        let lines = help_lines(&Config::parse("").unwrap());
        let mut out = Vec::new();
        paint_overlay(
            &mut out,
            Area {
                x: 0,
                y: 0,
                w: 12,
                h: 12,
            },
            &lines,
            "\x1b[36m",
            "",
        );
        let rendered = String::from_utf8(out).unwrap();
        assert!(rendered.contains("\x1b[90m."));
        assert!(rendered.contains("\x1b[0m help"));
        assert!(rendered.ends_with("\x1b[0m\x1b8"));
    }

    #[test]
    fn hotkey_overlay_lists_current_bindings_and_sanitizes_names() {
        let cfg = Config::parse("help_bind='alt+f1'\nreload_bind='alt+r'\n[[app]]\nname=\"test\\u001b[31m\"\nbind='ctrl+t'\ncmd=['test']").unwrap();
        let lines = help_lines(&cfg);
        assert_eq!(lines[0].text.len(), 32);
        assert!(lines[0].text.ends_with(". help"));
        assert!(lines[1].text.ends_with(". reload"));
        assert!(lines[2].text.contains("test"));
        let mut out = Vec::new();
        paint_overlay(
            &mut out,
            Area {
                x: 0,
                y: 0,
                w: 40,
                h: 8,
            },
            &lines,
            "\x1b[36m",
            "",
        );
        let rendered = String::from_utf8(out).unwrap();
        assert!(rendered.contains("\x1b[36malt+f1 \x1b[90m"));
        assert!(rendered.contains("\x1b[0m help"));
        assert!(!rendered.contains("test\x1b[31m"));
    }

    #[test]
    fn notification_is_bounded_and_sanitizes_control_characters() {
        let notification =
            Notification::new(NotificationKind::Error("a\x1b界\nb".into()), Instant::now());
        let mut out = Vec::new();
        paint_notification(
            &mut out,
            Area {
                x: 0,
                y: 0,
                w: 8,
                h: 6,
            },
            &notification,
        );
        let rendered = String::from_utf8(out).unwrap();
        assert!(rendered.contains("╭──────╮"));
        assert!(!rendered.contains("│      │"));
        assert!(rendered.contains("│ \x1b[31ma 界\x1b[90m │"));
        assert!(rendered.contains("│ \x1b[31mb\x1b[90m    │"));
        assert!(rendered.contains("╰──────╯"));
        assert!(!rendered.contains("\x1b界"));
        assert!(rendered.ends_with("\x1b[0m\x1b8"));
        let mut tiny = Vec::new();
        paint_notification(
            &mut tiny,
            Area {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            &notification,
        );
        assert!(String::from_utf8(tiny).unwrap().contains('…'));
    }

    #[test]
    fn notification_trims_error_whitespace_but_preserves_inner_indentation() {
        let notification = Notification::new(
            NotificationKind::Error(" \n error\n  detail \n\n".into()),
            Instant::now(),
        );
        let mut out = Vec::new();
        paint_notification(
            &mut out,
            Area {
                x: 0,
                y: 0,
                w: 30,
                h: 8,
            },
            &notification,
        );
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("✗ error"));
        assert!(text.contains("  detail"));
        assert!(text.contains("\x1b[4;"));
        assert!(!text.contains("\x1b[5;"));
    }

    #[test]
    fn notification_title_fits_and_disappears_on_narrow_terminals() {
        let notification =
            Notification::new(NotificationKind::Success("ok".into()), Instant::now());
        let mut out = Vec::new();
        paint_notification(
            &mut out,
            Area {
                x: 0,
                y: 0,
                w: 11,
                h: 3,
            },
            &notification,
        );
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("╭─ \x1b[1;36mhaunt\x1b[22;90m ─╮"));
        assert!(text.contains("╰─────────╯"));
        let mut narrow = Vec::new();
        paint_notification(
            &mut narrow,
            Area {
                x: 0,
                y: 0,
                w: 10,
                h: 3,
            },
            &notification,
        );
        assert!(!String::from_utf8(narrow).unwrap().contains("haunt"));
    }

    #[test]
    fn notification_wraps_and_marks_overflow() {
        let notification =
            Notification::new(NotificationKind::Error("abcdefghi".into()), Instant::now());
        let mut out = Vec::new();
        paint_notification(
            &mut out,
            Area {
                x: 0,
                y: 0,
                w: 7,
                h: 4,
            },
            &notification,
        );
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\x1b[31mabc\x1b[90m"));
        assert!(text.contains("\x1b[31m…\x1b[90m"));
    }
}

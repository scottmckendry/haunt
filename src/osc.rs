//! Small streaming OSC scanner for effects `libghostty-vt` does not expose
//! through its C API (progress reports and desktop notifications).
//!
//! This is deliberately not a general OSC parser: it recognizes only OSC 9;4
//! (ConEmu progress), OSC 9 messages, and OSC 777 notify. Everything else is
//! ignored. Sequences split across PTY reads are buffered.

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const MAX_OSC: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressState {
    Set,
    Error,
    Indeterminate,
    Pause,
}

impl ProgressState {
    /// Higher wins when several panes report progress.
    pub fn priority(self) -> u8 {
        match self {
            Self::Error => 3,
            Self::Indeterminate => 2,
            Self::Set | Self::Pause => 1,
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Set => 1,
            Self::Error => 2,
            Self::Indeterminate => 3,
            Self::Pause => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub state: ProgressState,
    pub percent: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressUpdate {
    Set(Progress),
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Progress(ProgressUpdate),
    /// A complete notification sequence, ready to forward verbatim.
    Notification(Vec<u8>),
}

#[derive(Debug, Default)]
pub struct Scanner {
    state: State,
    buf: Vec<u8>,
    overflow: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Esc,
    Osc,
    OscEsc,
}

impl Scanner {
    /// Feed raw PTY bytes and return any completed effects.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        for &byte in bytes {
            match self.state {
                State::Ground => {
                    if byte == ESC {
                        self.state = State::Esc;
                    }
                }
                State::Esc => match byte {
                    b']' => {
                        self.state = State::Osc;
                        self.buf.clear();
                        self.overflow = false;
                    }
                    ESC => {}
                    _ => self.state = State::Ground,
                },
                State::Osc => match byte {
                    BEL => {
                        self.finish(BEL, &mut events);
                        self.state = State::Ground;
                    }
                    ESC => self.state = State::OscEsc,
                    _ => {
                        if !self.overflow {
                            if self.buf.len() < MAX_OSC {
                                self.buf.push(byte);
                            } else {
                                self.overflow = true;
                                self.buf.clear();
                            }
                        }
                    }
                },
                State::OscEsc => {
                    if byte == b'\\' {
                        self.finish(b'\\', &mut events);
                    }
                    self.state = State::Ground;
                }
            }
        }
        events
    }

    fn finish(&mut self, terminator: u8, events: &mut Vec<Event>) {
        if self.overflow {
            self.overflow = false;
            return;
        }

        let mut parts = self.buf.split(|&b| b == b';');
        match parts.next() {
            Some(b"9") => match parts.next() {
                Some(b"4") => {
                    if let Some(update) = parse_progress(&mut parts) {
                        events.push(Event::Progress(update));
                    }
                }
                Some(_) => events.push(Event::Notification(sequence(&self.buf, terminator))),
                None => {}
            },
            Some(b"777") if parts.next() == Some(b"notify") => {
                events.push(Event::Notification(sequence(&self.buf, terminator)));
            }
            _ => {}
        }
    }
}

fn parse_progress<'a>(parts: &mut impl Iterator<Item = &'a [u8]>) -> Option<ProgressUpdate> {
    let state = parse_number(parts.next()?)?;
    let percent = parts.next().and_then(parse_number).map(|p| p.min(100));
    let state = match state {
        0 => return Some(ProgressUpdate::Remove),
        1 => ProgressState::Set,
        2 => ProgressState::Error,
        3 => ProgressState::Indeterminate,
        4 => ProgressState::Pause,
        _ => return None,
    };
    Some(ProgressUpdate::Set(Progress { state, percent }))
}

fn parse_number(bytes: &[u8]) -> Option<u8> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(
        bytes
            .iter()
            .fold(0u16, |n, &b| (n * 10 + (b - b'0') as u16).min(255)) as u8,
    )
}

fn sequence(body: &[u8], terminator: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 5);
    out.extend_from_slice(b"\x1b]");
    out.extend_from_slice(body);
    if terminator == b'\\' {
        out.extend_from_slice(b"\x1b\\");
    } else {
        out.push(BEL);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_split(scanner: &mut Scanner, chunks: &[&[u8]]) -> Vec<Event> {
        chunks
            .iter()
            .flat_map(|chunk| scanner.feed(chunk))
            .collect()
    }

    #[test]
    fn parses_progress_across_reads() {
        let mut scanner = Scanner::default();
        let events = feed_split(&mut scanner, &[b"\x1b]9;4;1;5", b"0\x07"]);
        assert_eq!(
            events,
            vec![Event::Progress(ProgressUpdate::Set(Progress {
                state: ProgressState::Set,
                percent: Some(50),
            }))]
        );
    }

    #[test]
    fn parses_every_progress_state() {
        let cases: &[(&[u8], ProgressUpdate)] = &[
            (b"\x1b]9;4;0\x07", ProgressUpdate::Remove),
            (
                b"\x1b]9;4;1;25\x07",
                ProgressUpdate::Set(Progress {
                    state: ProgressState::Set,
                    percent: Some(25),
                }),
            ),
            (
                b"\x1b]9;4;2\x07",
                ProgressUpdate::Set(Progress {
                    state: ProgressState::Error,
                    percent: None,
                }),
            ),
            (
                b"\x1b]9;4;3\x1b\\",
                ProgressUpdate::Set(Progress {
                    state: ProgressState::Indeterminate,
                    percent: None,
                }),
            ),
            (
                b"\x1b]9;4;4;75\x07",
                ProgressUpdate::Set(Progress {
                    state: ProgressState::Pause,
                    percent: Some(75),
                }),
            ),
            (
                b"\x1b]9;4;1;900\x07",
                ProgressUpdate::Set(Progress {
                    state: ProgressState::Set,
                    percent: Some(100),
                }),
            ),
        ];

        for (input, expected) in cases {
            let mut scanner = Scanner::default();
            assert_eq!(scanner.feed(input), vec![Event::Progress(*expected)]);
        }
    }

    #[test]
    fn repeated_progress_events_are_all_reported() {
        let mut scanner = Scanner::default();
        let events = scanner.feed(b"\x1b]9;4;3\x07\x1b]9;4;3\x07\x1b]9;4;0\x07");
        assert_eq!(events.len(), 3);
        assert!(matches!(
            events.as_slice(),
            [
                Event::Progress(ProgressUpdate::Set(_)),
                Event::Progress(ProgressUpdate::Set(_)),
                Event::Progress(ProgressUpdate::Remove),
            ]
        ));
    }

    #[test]
    fn forwards_notifications() {
        let mut scanner = Scanner::default();
        assert_eq!(
            scanner.feed(b"\x1b]9;build done\x07"),
            vec![Event::Notification(b"\x1b]9;build done\x07".to_vec())]
        );
        assert_eq!(
            scanner.feed(b"\x1b]777;notify;title;body\x1b\\"),
            vec![Event::Notification(
                b"\x1b]777;notify;title;body\x1b\\".to_vec()
            )]
        );
        assert!(scanner.feed(b"\x1b]777;precmd\x07").is_empty());
    }

    #[test]
    fn ignores_noise_and_malformed_progress() {
        let mut scanner = Scanner::default();
        assert!(scanner.feed(b"plain text\x1b[31m").is_empty());
        assert!(scanner.feed(b"\x1b]9;4;x;1\x07").is_empty());
        assert!(scanner.feed(b"\x1b]9;4;9\x07").is_empty());
        assert!(scanner.feed(b"\x1b]52;c;Zm9v\x07").is_empty());
    }

    #[test]
    fn drops_oversized_sequences() {
        let mut scanner = Scanner::default();
        let mut input = b"\x1b]9;".to_vec();
        input.extend(std::iter::repeat_n(b'x', MAX_OSC + 10));
        input.push(BEL);
        assert!(scanner.feed(&input).is_empty());
    }
}

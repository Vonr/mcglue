#![allow(unused)]

use std::{
    borrow::Cow,
    fmt::{Debug, Display},
    ops::Deref,
    sync::{LazyLock, OnceLock},
};

use bstr::ByteSlice;
use chumsky::{prelude::*, span::Span};
use futures::future::Lazy;

use crate::{DEATH_MESSAGES, DeathMessageComponent};

type LoggerParserExtra<'src> = extra::Full<Rich<'src, u8>, (), Logger<'src>>;

enum PartialLog<'src> {
    Generic {
        message: &'src [u8],
    },
    Chat {
        secure: bool,
        sender: &'src [u8],
        message: &'src [u8],
    },
    Join {
        player: &'src [u8],
    },
    Leave {
        player: &'src [u8],
    },
    Advancement {
        player: &'src [u8],
        advancement: &'src [u8],
    },
    Death {
        victim: &'src [u8],
        attacker: &'src [u8],
        weapon: &'src [u8],
    },
    Unknown {
        message: &'src [u8],
    },
}

#[derive(Clone, Debug)]
pub enum Log<'src> {
    Generic(GenericLog<'src>),
    Chat(ChatLog<'src>),
    Join(JoinLog<'src>),
    Leave(LeaveLog<'src>),
    Advancement(AdvancementLog<'src>),
    Death(DeathLog<'src>),
    Unknown(ShowLossyStr<'src>),
}

#[repr(transparent)]
#[derive(Clone, PartialEq, Eq, Default)]
pub struct ShowLossyStr<'a>(pub &'a [u8]);

impl<'a> ShowLossyStr<'a> {
    pub fn to_str_lossy(&self) -> Cow<'_, str> {
        self.0.to_str_lossy()
    }
}

impl Debug for ShowLossyStr<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.to_str_lossy())
    }
}

impl Display for ShowLossyStr<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self, f)
    }
}

impl<'a> Deref for ShowLossyStr<'a> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl<'src> Log<'src> {
    pub fn parser()
    -> impl Parser<'src, &'src [u8], (Self, SimpleSpan<usize>), extra::Err<Rich<'src, u8>>> {
        trait OnlyIfLogger<'src, I, O, E, P>
        where
            I: Input<'src>,
            E: extra::ParserExtra<'src, I, Context = Logger<'src>>,
            P: Parser<'src, I, O, E>,
        {
            fn only_if_logger(self, level: LogLevel, name: &[u8]) -> impl Parser<'src, I, O, E>;
        }

        impl<'src, I, O, E, P> OnlyIfLogger<'src, I, O, E, P> for P
        where
            I: Input<'src>,
            E: extra::ParserExtra<'src, I, Context = Logger<'src>>,
            P: Parser<'src, I, O, E>,
        {
            fn only_if_logger(self, level: LogLevel, name: &[u8]) -> impl Parser<'src, I, O, E> {
                self.contextual().configure(move |_, ctx: &Logger<'src>| {
                    ctx.level == level && ctx.name.0 == name
                })
            }
        }

        let non_whitespace_slice = any::<'src, &'src [u8], LoggerParserExtra<'src>>()
            .filter(|b: &u8| !b.is_ascii_whitespace())
            .repeated()
            .at_least(1)
            .map_err(|e| Rich::custom(*e.span(), "Could not find non-whitespace slice"))
            .to_slice();

        let chat = group((
            just::<'src, _, _, LoggerParserExtra<'src>>(b"Not Secure")
                .delimited_by(just(b'['), just(b']'))
                .or_not()
                .map(|o| o.is_none())
                .then_ignore(just(b' ')),
            choice((
                any()
                    .filter(|b| *b != b'>')
                    .repeated()
                    .at_least(1)
                    .to_slice()
                    .delimited_by(just(b'<'), just(b'>')),
                any()
                    .filter(|b| *b != b']')
                    .repeated()
                    .at_least(1)
                    .delimited_by(just(b'['), just(b']'))
                    .to_slice(),
            ))
            .then_ignore(just(b' ')),
            any().repeated().at_least(1).to_slice(),
        ))
        .map_err(|e| Rich::custom(*e.span(), "Could not parse as chat message"))
        .map(|(secure, sender, message)| PartialLog::Chat {
            secure,
            sender,
            message,
        })
        .only_if_logger(LogLevel::Info, b"Server thread");

        let join = non_whitespace_slice
            .then_ignore(just(b" joined the game"))
            .map(|player| PartialLog::Join { player })
            .map_err(|e| Rich::custom(*e.span(), "Could not parse as join message"))
            .only_if_logger(LogLevel::Info, b"Server thread");

        let leave = non_whitespace_slice
            .then_ignore(just(b" left the game"))
            .map(|player| PartialLog::Leave { player })
            .map_err(|e| Rich::custom(*e.span(), "Could not parse as leave message"))
            .only_if_logger(LogLevel::Info, b"Server thread");

        let advancement = non_whitespace_slice
            .then_ignore(choice((
                just(b" has made the advancement ".as_slice()),
                just(b" has reached the goal ".as_slice()),
                just(b" has completed the challenge ".as_slice()),
            )))
            .then(
                any()
                    .filter(|b| *b != b']')
                    .repeated()
                    .at_least(1)
                    .to_slice()
                    .delimited_by(just(b'['), just(b']')),
            )
            .map(|(player, advancement)| PartialLog::Advancement {
                player,
                advancement,
            })
            .map_err(|e| Rich::custom(*e.span(), "Could not parse as advancement message"))
            .only_if_logger(LogLevel::Info, b"Server thread");

        let death = custom::<_, &[u8], _, LoggerParserExtra<'src>>(move |inp| {
            let cursor = inp.cursor();
            'death_message: for &(a, ar, b, br, c, cr, d) in DEATH_MESSAGES.get().unwrap().iter() {
                let mut slice = inp.slice_from(&cursor..);

                if !a.is_empty() {
                    let Ok(span) = just::<_, _, LoggerParserExtra<'src>>(a)
                        .map_with(|_, e| e.span())
                        .lazy()
                        .parse(slice)
                        .into_result()
                        .map_err(|mut e| e.swap_remove(0))
                    else {
                        continue;
                    };

                    slice = &slice[span.end..];
                }

                let mut ret = (b"".as_slice(), b"".as_slice(), b"".as_slice());

                for (component_type, postfix) in [(ar, b), (br, c), (cr, d)] {
                    if matches!(component_type, DeathMessageComponent::Empty) && postfix.is_empty()
                    {
                        continue;
                    }

                    if !matches!(component_type, DeathMessageComponent::Empty) {
                        let mut component_len = if postfix.is_empty() {
                            slice.len()
                        } else {
                            let mut component_len = 1;
                            while slice.len() - component_len >= postfix.len()
                                && !slice[component_len..].starts_with(postfix)
                            {
                                component_len += 1;
                            }
                            component_len
                        };

                        let component;
                        (component, slice) = slice.split_at(component_len);

                        match component_type {
                            DeathMessageComponent::Victim => {
                                ret.0 = component;
                            }
                            DeathMessageComponent::Attacker => {
                                ret.1 = component;
                            }
                            DeathMessageComponent::Weapon => {
                                ret.2 = component;
                            }
                            DeathMessageComponent::Empty => unreachable!(),
                        }
                    }

                    if !postfix.is_empty() {
                        let Ok(span) = just::<_, _, LoggerParserExtra<'src>>(postfix)
                            .map_with(|_, e| e.span())
                            .lazy()
                            .parse(slice)
                            .into_result()
                            .map_err(|mut e| e.swap_remove(0))
                        else {
                            continue 'death_message;
                        };

                        slice = &slice[span.end..];
                    }
                }

                while inp.next().is_some() {}
                return Ok(ret);
            }

            Err(Rich::custom(
                inp.span_from(&inp.cursor()..),
                "Could not parse as a death message",
            ))
        })
        .map(|(victim, attacker, weapon)| PartialLog::Death {
            victim,
            attacker,
            weapon,
        })
        .map_err(|e| Rich::custom(*e.span(), "Could not parse as a death message"))
        .only_if_logger(LogLevel::Info, b"Server thread");

        let generic = any()
            .repeated()
            .at_least(1)
            .to_slice()
            .map(|message| PartialLog::Generic { message });

        let partial_logs = choice((chat, join, leave, advancement, death, generic));

        group((
            HmsTime::parser()
                .delimited_by(just(b'['), just(b']'))
                .then_ignore(just(b' ')),
            Logger::parser()
                .then_ignore(just(b": ".as_slice()))
                .then_with_ctx(partial_logs.map_with(|parsed, e| (parsed, e.span()))),
        ))
        .map(|(time, (logger, (partial, span)))| {
            (
                match partial {
                    PartialLog::Generic { message } => Self::Generic(GenericLog {
                        time,
                        logger,
                        message: ShowLossyStr(message),
                    }),
                    PartialLog::Chat {
                        secure,
                        sender,
                        message,
                    } => Self::Chat(ChatLog {
                        time,
                        secure,
                        sender: ShowLossyStr(sender),
                        message: ShowLossyStr(message),
                    }),
                    PartialLog::Join { player } => Self::Join(JoinLog {
                        time,
                        player: ShowLossyStr(player),
                    }),
                    PartialLog::Leave { player } => Self::Leave(LeaveLog {
                        time,
                        player: ShowLossyStr(player),
                    }),
                    PartialLog::Advancement {
                        player,
                        advancement,
                    } => Self::Advancement(AdvancementLog {
                        time,
                        player: ShowLossyStr(player),
                        advancement: ShowLossyStr(advancement),
                    }),
                    PartialLog::Death {
                        victim,
                        attacker,
                        weapon,
                    } => Self::Death(DeathLog {
                        time,
                        victim: ShowLossyStr(victim),
                        attacker: ShowLossyStr(attacker),
                        weapon: ShowLossyStr(weapon),
                    }),
                    PartialLog::Unknown { message } => Self::Unknown(ShowLossyStr(message)),
                },
                span,
            )
        })
        .or(any()
            .repeated()
            .lazy()
            .to_slice()
            .map_with(|unknown, e| (Log::Unknown(ShowLossyStr(unknown)), e.span())))
    }
}

#[derive(Clone, Debug)]
pub struct GenericLog<'src> {
    pub time: HmsTime,
    pub logger: Logger<'src>,
    pub message: ShowLossyStr<'src>,
}

#[derive(Clone, Debug)]
pub struct ChatLog<'src> {
    pub time: HmsTime,
    pub secure: bool,
    pub sender: ShowLossyStr<'src>,
    pub message: ShowLossyStr<'src>,
}

#[derive(Clone, Debug)]
pub struct JoinLog<'src> {
    pub time: HmsTime,
    pub player: ShowLossyStr<'src>,
}

#[derive(Clone, Debug)]
pub struct LeaveLog<'src> {
    pub time: HmsTime,
    pub player: ShowLossyStr<'src>,
}

#[derive(Clone, Debug)]
pub struct AdvancementLog<'src> {
    pub time: HmsTime,
    pub player: ShowLossyStr<'src>,
    pub advancement: ShowLossyStr<'src>,
}

#[derive(Clone, Debug)]
pub struct DeathLog<'src> {
    pub time: HmsTime,
    pub victim: ShowLossyStr<'src>,
    pub attacker: ShowLossyStr<'src>,
    pub weapon: ShowLossyStr<'src>,
}

#[derive(Clone, Copy, Debug)]
pub struct HmsTime {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
}

impl Display for HmsTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:02}:{:02}:{:02}",
            self.hours, self.minutes, self.seconds
        )
    }
}

impl HmsTime {
    pub fn parser<'src>() -> impl Parser<'src, &'src [u8], HmsTime, extra::Err<Rich<'src, u8>>> {
        let colon = just(b':').ignored();
        let digit = any().filter(u8::is_ascii_digit);
        let digit_pair = digit.then(digit).map(|(a, b)| (a - b'0') * 10 + b - b'0');

        group((digit_pair, colon, digit_pair, colon, digit_pair)).map(
            |(hours, _, minutes, _, seconds)| HmsTime {
                hours,
                minutes,
                seconds,
            },
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Logger<'src> {
    pub name: ShowLossyStr<'src>,
    pub level: LogLevel,
}

impl Display for Logger<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.name, self.level)
    }
}

impl<'src> Logger<'src> {
    pub fn parser() -> impl Parser<'src, &'src [u8], Logger<'src>, extra::Err<Rich<'src, u8>>> {
        let not_slash = any().filter(|b: &u8| *b != b'/');

        group((
            not_slash.repeated().to_slice(),
            just(b'/').ignored(),
            LogLevel::parser(),
        ))
        .delimited_by(just(b'['), just(b']'))
        .map(|(name, _, level)| Logger {
            name: ShowLossyStr(name),
            level,
        })
    }
}

#[derive(PartialEq, Eq, Debug, Clone, Copy, Default)]
pub enum LogLevel {
    #[default]
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self, f)
    }
}

impl LogLevel {
    pub fn parser<'src>() -> impl Parser<'src, &'src [u8], LogLevel, extra::Err<Rich<'src, u8>>> {
        choice((
            text::keyword::<_, _, extra::Err<Rich<u8>>>(b"TRACE").to(LogLevel::Trace),
            text::keyword(b"DEBUG").to(LogLevel::Debug),
            text::keyword(b"INFO").to(LogLevel::Info),
            text::keyword(b"WARN").to(LogLevel::Warn),
            text::keyword(b"ERROR").to(LogLevel::Error),
            text::keyword(b"FATAL").to(LogLevel::Fatal),
        ))
    }
}

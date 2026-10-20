//! The `content` and `quotes` properties of the boxes the style generates:
//! what a `::before` or `::after` holds, and the quotation marks an
//! `open-quote` and a `close-quote` stand for.

use std::sync::Arc;

/// One piece of a `content` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentItem {
    Text(Arc<str>),
    /// `attr(name)`: the value of the attribute of the element the box belongs to.
    Attr(Arc<str>),
    OpenQuote,
    CloseQuote,
    NoOpenQuote,
    NoCloseQuote,
    /// `counter(name, style)`.
    Counter {
        name: Arc<str>,
        style: Arc<str>,
    },
    /// `counters(name, separator, style)`.
    Counters {
        name: Arc<str>,
        separator: Arc<str>,
        style: Arc<str>,
    },
    /// `url(...)`.
    Image(Arc<str>),
}

/// `content`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Content {
    /// The initial value: nothing is generated.
    #[default]
    Normal,
    None,
    Items(Arc<[ContentItem]>),
}

impl Content {
    /// Whether a `::before` or `::after` with this value has a box.
    pub fn generates_box(&self) -> bool {
        matches!(self, Content::Items(_))
    }
}

/// `quotes`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Quotes {
    /// The marks of the language, which is English: “ ” then ‘ ’.
    #[default]
    Auto,
    None,
    /// The open and close mark of each depth; the last pair serves every
    /// deeper level.
    Pairs(Arc<[(Arc<str>, Arc<str>)]>),
}

impl Quotes {
    /// The pair for quotation depth `depth` (0 outermost).
    pub fn pair(&self, depth: usize) -> Option<(&str, &str)> {
        const ENGLISH: [(&str, &str); 2] = [("\u{201c}", "\u{201d}"), ("\u{2018}", "\u{2019}")];
        match self {
            Quotes::None => None,
            Quotes::Auto => Some(ENGLISH[depth.min(1)]),
            Quotes::Pairs(pairs) => pairs
                .get(depth)
                .or_else(|| pairs.last())
                .map(|(open, close)| (&**open, &**close)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_quotes_are_english_and_the_second_pair_serves_every_deeper_level() {
        let quotes = Quotes::Auto;
        assert_eq!(quotes.pair(0), Some(("\u{201c}", "\u{201d}")));
        assert_eq!(quotes.pair(1), Some(("\u{2018}", "\u{2019}")));
        assert_eq!(quotes.pair(5), quotes.pair(1));
    }

    #[test]
    fn explicit_pairs_run_out_onto_the_last_one_and_none_has_no_mark() {
        let pairs = Quotes::Pairs(Arc::from([(Arc::from("<"), Arc::from(">"))]));
        assert_eq!(pairs.pair(3), Some(("<", ">")));
        assert_eq!(Quotes::None.pair(0), None);
    }
}

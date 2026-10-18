//! The longhands of `mask-*` that the style engine's servo build does not know
//! (`mask-mode`, `-repeat`, `-position`, `-size`, `-origin`, `-clip` and
//! `-composite`), read back from the custom properties
//! [`crate::property_mirror`] carries them in. Each is a comma-separated list,
//! one entry per layer; a list that does not parse is dropped whole, like any
//! invalid declaration. `mask-image` itself is the engine's.

use cssparser::{Parser, ParserInput};

use crate::background::{BackgroundBox, BackgroundRepeat, BackgroundSize, MaskComposite, MaskMode};
use crate::cascade::LengthPercentage;
use crate::shape_function::parse_length;

type Fail<'i> = cssparser::ParseError<'i, ()>;

/// Every comma-separated entry of `css`, each read by `entry`; `None` when any
/// is invalid or something is left over.
fn list<T>(
    css: &str,
    entry: impl for<'i> Fn(&mut Parser<'i, '_>) -> Result<T, Fail<'i>>,
) -> Option<Vec<T>> {
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    parser
        .parse_comma_separated(|p| {
            let value = entry(p)?;
            p.expect_exhausted()?;
            Ok(value)
        })
        .ok()
}

fn keyword<'i>(p: &mut Parser<'i, '_>) -> Result<String, Fail<'i>> {
    Ok(p.expect_ident()?.to_ascii_lowercase())
}

pub(crate) fn parse_modes(css: &str) -> Option<Vec<MaskMode>> {
    list(css, |p| match &*keyword(p)? {
        "match-source" => Ok(MaskMode::MatchSource),
        "alpha" => Ok(MaskMode::Alpha),
        "luminance" => Ok(MaskMode::Luminance),
        _ => Err(p.new_custom_error(())),
    })
}

pub(crate) fn parse_composites(css: &str) -> Option<Vec<MaskComposite>> {
    list(css, |p| match &*keyword(p)? {
        "add" => Ok(MaskComposite::Add),
        "subtract" => Ok(MaskComposite::Subtract),
        "intersect" => Ok(MaskComposite::Intersect),
        "exclude" => Ok(MaskComposite::Exclude),
        _ => Err(p.new_custom_error(())),
    })
}

/// A box of `mask-origin` or `mask-clip`. `fill-box` is the content box and
/// `stroke-box`, `view-box` and `margin-box` the border box (what Chrome makes
/// of them for an element with a CSS box); `no-clip` is only a clip.
pub(crate) fn parse_boxes(css: &str, allow_no_clip: bool) -> Option<Vec<BackgroundBox>> {
    list(css, |p| match &*keyword(p)? {
        "border-box" | "margin-box" | "stroke-box" | "view-box" => Ok(BackgroundBox::Border),
        "padding-box" => Ok(BackgroundBox::Padding),
        "content-box" | "fill-box" => Ok(BackgroundBox::Content),
        "no-clip" if allow_no_clip => Ok(BackgroundBox::NoClip),
        _ => Err(p.new_custom_error(())),
    })
}

pub(crate) fn parse_repeats(css: &str) -> Option<Vec<(BackgroundRepeat, BackgroundRepeat)>> {
    list(css, |p| {
        let one = |word: &str| -> Option<BackgroundRepeat> {
            Some(match word {
                "repeat" => BackgroundRepeat::Repeat,
                "space" => BackgroundRepeat::Space,
                "round" => BackgroundRepeat::Round,
                "no-repeat" => BackgroundRepeat::NoRepeat,
                _ => return None,
            })
        };
        let first = keyword(p)?;
        match &*first {
            "repeat-x" => return Ok((BackgroundRepeat::Repeat, BackgroundRepeat::NoRepeat)),
            "repeat-y" => return Ok((BackgroundRepeat::NoRepeat, BackgroundRepeat::Repeat)),
            _ => {}
        }
        let x = one(&first).ok_or_else(|| p.new_custom_error(()))?;
        let y = match p.try_parse(keyword) {
            Ok(second) => one(&second).ok_or_else(|| p.new_custom_error(()))?,
            Err(_) => x,
        };
        Ok((x, y))
    })
}

pub(crate) fn parse_sizes(css: &str, font_size: f32) -> Option<Vec<BackgroundSize>> {
    list(css, |p| {
        if let Ok(word) = p.try_parse(keyword) {
            return match &*word {
                "cover" => Ok(BackgroundSize::Cover),
                "contain" => Ok(BackgroundSize::Contain),
                "auto" => {
                    let height = axis(p, font_size)?;
                    Ok(BackgroundSize::Explicit(None, height))
                }
                _ => Err(p.new_custom_error(())),
            };
        }
        let width = parse_length(p, font_size)?;
        let height = axis(p, font_size)?;
        Ok(BackgroundSize::Explicit(Some(width), height))
    })
}

/// The optional second axis of a size: `auto` (or nothing) is `None`.
fn axis<'i>(p: &mut Parser<'i, '_>, font_size: f32) -> Result<Option<LengthPercentage>, Fail<'i>> {
    if p.is_exhausted() {
        return Ok(None);
    }
    if p.try_parse(|p| p.expect_ident_matching("auto")).is_ok() {
        return Ok(None);
    }
    parse_length(p, font_size).map(Some)
}

/// One item of a position: a keyword or a length.
enum Item {
    Keyword(String),
    Length(LengthPercentage),
}

fn percent(fraction: f32) -> LengthPercentage {
    LengthPercentage {
        length: 0.0,
        percentage: fraction,
    }
}

/// `mask-position`: one to four items, `left`/`right`/`top`/`bottom` taking an
/// offset from their edge (`right 10px` is `100% - 10px`), as `background-position`.
pub(crate) fn parse_positions(
    css: &str,
    font_size: f32,
) -> Option<Vec<(LengthPercentage, LengthPercentage)>> {
    list(css, |p| {
        let mut items: Vec<Item> = Vec::new();
        while !p.is_exhausted() && items.len() < 4 {
            if let Ok(word) = p.try_parse(keyword) {
                items.push(Item::Keyword(word));
            } else {
                items.push(Item::Length(parse_length(p, font_size)?));
            }
        }
        position_of(items).ok_or_else(|| p.new_custom_error(()))
    })
}

fn position_of(items: Vec<Item>) -> Option<(LengthPercentage, LengthPercentage)> {
    let horizontal = |w: &str| matches!(w, "left" | "right");
    let vertical = |w: &str| matches!(w, "top" | "bottom");
    let edge = |w: &str, offset: Option<LengthPercentage>| -> Option<LengthPercentage> {
        let offset = offset.unwrap_or_default();
        match w {
            "left" | "top" => Some(offset),
            "right" | "bottom" => Some(LengthPercentage {
                length: -offset.length,
                percentage: 1.0 - offset.percentage,
            }),
            "center" => offset
                .eq(&LengthPercentage::default())
                .then(|| percent(0.5)),
            _ => None,
        }
    };
    match items.as_slice() {
        [Item::Length(x)] => Some((*x, percent(0.5))),
        [Item::Keyword(w)] if vertical(w) => Some((percent(0.5), edge(w, None)?)),
        [Item::Keyword(w)] => Some((edge(w, None)?, percent(0.5))),
        [a, b] => {
            // Keywords may come in either order when both name an axis.
            let swap = matches!(a, Item::Keyword(w) if vertical(w))
                || matches!(b, Item::Keyword(w) if horizontal(w));
            let (x, y) = if swap { (b, a) } else { (a, b) };
            let value = |item: &Item, axis_ok: &dyn Fn(&str) -> bool| match item {
                Item::Length(lp) => Some(*lp),
                Item::Keyword(w) if axis_ok(w) || w == "center" => edge(w, None),
                Item::Keyword(_) => None,
            };
            Some((value(x, &|w| horizontal(w))?, value(y, &|w| vertical(w))?))
        }
        [_, _, _] | [_, _, _, _] => {
            // Keyword, optional offset, keyword, optional offset.
            let mut groups: Vec<(String, Option<LengthPercentage>)> = Vec::new();
            for item in items {
                match item {
                    Item::Keyword(w) => groups.push((w, None)),
                    Item::Length(lp) => {
                        let last = groups.last_mut()?;
                        if last.1.is_some() {
                            return None;
                        }
                        last.1 = Some(lp);
                    }
                }
            }
            let [(a, a_offset), (b, b_offset)] = <[_; 2]>::try_from(groups).ok()?;
            let swap = vertical(&a) || horizontal(&b);
            let ((x, x_offset), (y, y_offset)) = if swap {
                ((b, b_offset), (a, a_offset))
            } else {
                ((a, a_offset), (b, b_offset))
            };
            if vertical(&x) || horizontal(&y) {
                return None;
            }
            Some((edge(&x, x_offset)?, edge(&y, y_offset)?))
        }
        _ => None,
    }
}

/// `background-blend-mode`: a blend mode per layer, which the engine's servo
/// build does not know either.
pub(crate) fn parse_blend_modes(css: &str) -> Option<Vec<crate::background::BlendMode>> {
    use crate::background::BlendMode;
    list(css, |p| {
        Ok(match &*keyword(p)? {
            "normal" => BlendMode::Normal,
            "multiply" => BlendMode::Multiply,
            "screen" => BlendMode::Screen,
            "overlay" => BlendMode::Overlay,
            "darken" => BlendMode::Darken,
            "lighten" => BlendMode::Lighten,
            "color-dodge" => BlendMode::ColorDodge,
            "color-burn" => BlendMode::ColorBurn,
            "hard-light" => BlendMode::HardLight,
            "soft-light" => BlendMode::SoftLight,
            "difference" => BlendMode::Difference,
            "exclusion" => BlendMode::Exclusion,
            "hue" => BlendMode::Hue,
            "saturation" => BlendMode::Saturation,
            "color" => BlendMode::Color,
            "luminosity" => BlendMode::Luminosity,
            "plus-lighter" => BlendMode::PlusLighter,
            _ => return Err(p.new_custom_error(())),
        })
    })
}

//! `clip-path: shape(...)`, which the style engine's servo build refuses (its
//! preference for the function is compiled off). [`crate::property_mirror`]
//! carries such a declaration as the custom property `--florui-clip-path`, and
//! this reads it back: the grammar of CSS Shapes 2,
//!
//! ```text
//! shape( <fill-rule>? from <pair>, <command># )   with an optional <geometry-box>
//! move|line to|by <pair>     hline|vline to|by <length-percentage>
//! curve to|by <pair> with <pair> [/ <pair>]      (one control point: quadratic)
//! smooth to|by <pair> [with <pair>]              (no control point: quadratic)
//! arc to|by <pair> of <lp> [<lp>] [cw|ccw] [small|large] [rotate <angle>]
//! close
//! ```
//!
//! A value that does not parse is dropped whole, like any invalid declaration.
//! Lengths are `px`, the absolute units, `em`, `rem` (against 16px: the root
//! font size is not at hand here), `calc()` of those and percentages; the
//! viewport units are not understood.

use cssparser::{Parser, ParserInput, Token};

use crate::cascade::LengthPercentage;
use crate::clip::{ClipPath, ClipShape, Point, RawCommand, ReferenceBox, absolutize};

/// What a length can be relative to: the element's font size and the viewport.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Units {
    pub font_size: f32,
    /// The viewport's width and height in pixels.
    pub viewport: (f32, f32),
}

impl Units {
    pub(crate) fn new(font_size: f32, viewport: (f32, f32)) -> Self {
        Self {
            font_size,
            viewport,
        }
    }
}

/// Where the root font size would come from if it were at hand.
const ROOT_FONT_SIZE: f32 = 16.0;

type Fail<'i> = cssparser::ParseError<'i, ()>;

/// The `clip-path` value `css` holds, when it is a `shape()` (alone or with a
/// reference box on either side); `None` for anything else, invalid ones too.
pub(crate) fn parse_clip_path(css: &str, units: Units) -> Option<ClipPath> {
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    let mut shape: Option<ClipShape> = None;
    let mut reference: Option<ReferenceBox> = None;
    while !parser.is_exhausted() {
        let next = parser.next().ok()?.clone();
        match next {
            Token::Function(name) if name.eq_ignore_ascii_case("shape") && shape.is_none() => {
                shape = Some(
                    parser
                        .parse_nested_block(|inner| parse_shape(inner, units))
                        .ok()?,
                );
            }
            Token::Ident(word) if reference.is_none() => {
                reference = Some(match &*word.to_ascii_lowercase() {
                    "margin-box" => ReferenceBox::Margin,
                    "border-box" | "stroke-box" | "view-box" => ReferenceBox::Border,
                    "padding-box" => ReferenceBox::Padding,
                    "content-box" | "fill-box" => ReferenceBox::Content,
                    _ => return None,
                });
            }
            _ => return None,
        }
    }
    Some(ClipPath::Shape(
        Box::new(shape?),
        reference.unwrap_or(ReferenceBox::Border),
    ))
}

fn parse_shape<'i>(p: &mut Parser<'i, '_>, units: Units) -> Result<ClipShape, Fail<'i>> {
    let mut even_odd = false;
    if let Ok(rule) = p.try_parse(|p| -> Result<bool, Fail<'i>> {
        let ident = p.expect_ident()?.clone();
        match &*ident.to_ascii_lowercase() {
            "nonzero" => Ok(false),
            "evenodd" => Ok(true),
            _ => Err(p.new_custom_error(())),
        }
    }) {
        even_odd = rule;
    }
    p.expect_ident_matching("from")?;
    let mut commands = vec![RawCommand::Move {
        abs: true,
        to: parse_pair(p, units)?,
    }];
    while !p.is_exhausted() {
        p.expect_comma()?;
        commands.push(parse_command(p, units)?);
    }
    Ok(ClipShape::Path {
        even_odd,
        segments: absolutize(&commands),
    })
}

fn parse_command<'i>(p: &mut Parser<'i, '_>, units: Units) -> Result<RawCommand, Fail<'i>> {
    let name = p.expect_ident()?.to_ascii_lowercase();
    if name == "close" {
        return Ok(RawCommand::Close);
    }
    let abs = {
        let by_to = p.expect_ident()?.to_ascii_lowercase();
        match &*by_to {
            "to" => true,
            "by" => false,
            _ => return Err(p.new_custom_error(())),
        }
    };
    match &*name {
        "move" => Ok(RawCommand::Move {
            abs,
            to: parse_pair(p, units)?,
        }),
        "line" => Ok(RawCommand::Line {
            abs,
            to: parse_pair(p, units)?,
        }),
        "hline" => Ok(RawCommand::HLine {
            abs,
            x: parse_length(p, units)?,
        }),
        "vline" => Ok(RawCommand::VLine {
            abs,
            y: parse_length(p, units)?,
        }),
        "curve" => {
            let to = parse_pair(p, units)?;
            p.expect_ident_matching("with")?;
            let c1 = parse_pair(p, units)?;
            if p.try_parse(|p| p.expect_delim('/')).is_ok() {
                let c2 = parse_pair(p, units)?;
                Ok(RawCommand::Cubic { abs, to, c1, c2 })
            } else {
                Ok(RawCommand::Quad { abs, to, c1 })
            }
        }
        "smooth" => {
            let to = parse_pair(p, units)?;
            if p.try_parse(|p| p.expect_ident_matching("with")).is_ok() {
                let c2 = parse_pair(p, units)?;
                Ok(RawCommand::SmoothCubic { abs, to, c2 })
            } else {
                Ok(RawCommand::SmoothQuad { abs, to })
            }
        }
        "arc" => {
            let to = parse_pair(p, units)?;
            p.expect_ident_matching("of")?;
            let rx = parse_length(p, units)?;
            let ry = p.try_parse(|p| parse_length(p, units)).unwrap_or(rx);
            let (mut clockwise, mut large, mut rotation) = (false, false, 0.0);
            // The three options may come in any order, each once.
            let (mut sweep_seen, mut size_seen, mut rotate_seen) = (false, false, false);
            while let Ok(word) = p.try_parse(|p| p.expect_ident().map(|w| w.to_ascii_lowercase())) {
                match &*word {
                    "cw" if !sweep_seen => {
                        clockwise = true;
                        sweep_seen = true;
                    }
                    "ccw" if !sweep_seen => sweep_seen = true,
                    "large" if !size_seen => {
                        large = true;
                        size_seen = true;
                    }
                    "small" if !size_seen => size_seen = true,
                    "rotate" if !rotate_seen => {
                        rotation = parse_angle(p)?;
                        rotate_seen = true;
                    }
                    _ => return Err(p.new_custom_error(())),
                }
            }
            Ok(RawCommand::Arc {
                abs,
                to,
                radii: (rx, ry),
                rotation,
                large,
                clockwise,
            })
        }
        _ => Err(p.new_custom_error(())),
    }
}

fn parse_pair<'i>(p: &mut Parser<'i, '_>, units: Units) -> Result<Point, Fail<'i>> {
    Ok((parse_length(p, units)?, parse_length(p, units)?))
}

fn parse_angle<'i>(p: &mut Parser<'i, '_>) -> Result<f32, Fail<'i>> {
    let token = p.next()?.clone();
    match token {
        Token::Dimension { value, unit, .. } => match &*unit.to_ascii_lowercase() {
            "deg" => Ok(value),
            "grad" => Ok(value * 0.9),
            "rad" => Ok(value.to_degrees()),
            "turn" => Ok(value * 360.0),
            _ => Err(p.new_custom_error(())),
        },
        Token::Number { value: 0.0, .. } => Ok(0.0),
        _ => Err(p.new_custom_error(())),
    }
}

/// One `<length-percentage>`.
pub(crate) fn parse_length<'i>(
    p: &mut Parser<'i, '_>,
    units: Units,
) -> Result<LengthPercentage, Fail<'i>> {
    let token = p.next()?.clone();
    match token {
        Token::Function(name) if name.eq_ignore_ascii_case("calc") => {
            let sum = p.parse_nested_block(|inner| parse_sum(inner, units))?;
            // A bare number is not a length.
            if sum.percentage.is_nan() {
                return Err(p.new_custom_error(()));
            }
            Ok(sum)
        }
        other => leaf(&other, units).ok_or_else(|| p.new_custom_error(())),
    }
}

/// A length, a percentage or a bare zero.
fn leaf(token: &Token<'_>, units: Units) -> Option<LengthPercentage> {
    let px = |length: f32| LengthPercentage {
        length,
        percentage: 0.0,
    };
    match token {
        Token::Percentage { unit_value, .. } => Some(LengthPercentage {
            length: 0.0,
            percentage: *unit_value,
        }),
        Token::Number { value, .. } if *value == 0.0 => Some(px(0.0)),
        Token::Dimension { value, unit, .. } => {
            let per_unit = match &*unit.to_ascii_lowercase() {
                "px" => 1.0,
                "em" => units.font_size,
                "rem" => ROOT_FONT_SIZE,
                "in" => 96.0,
                "cm" => 96.0 / 2.54,
                "mm" => 96.0 / 25.4,
                "q" => 96.0 / 101.6,
                "pt" => 96.0 / 72.0,
                "pc" => 16.0,
                // Small, large and dynamic viewports are all the one viewport here.
                "vw" | "svw" | "lvw" | "dvw" | "vi" | "svi" | "lvi" | "dvi" => {
                    units.viewport.0 / 100.0
                }
                "vh" | "svh" | "lvh" | "dvh" | "vb" | "svb" | "lvb" | "dvb" => {
                    units.viewport.1 / 100.0
                }
                "vmin" | "svmin" | "lvmin" | "dvmin" => {
                    units.viewport.0.min(units.viewport.1) / 100.0
                }
                "vmax" | "svmax" | "lvmax" | "dvmax" => {
                    units.viewport.0.max(units.viewport.1) / 100.0
                }
                _ => return None,
            };
            Some(px(value * per_unit))
        }
        _ => None,
    }
}

/// The inside of a `calc()`: terms added and subtracted, each a length or a
/// percentage times and divided by plain numbers, or a parenthesized sum.
fn parse_sum<'i>(p: &mut Parser<'i, '_>, units: Units) -> Result<LengthPercentage, Fail<'i>> {
    let mut total = parse_product(p, units)?;
    while !p.is_exhausted() {
        let sign = match p.next()?.clone() {
            Token::Delim('+') => 1.0,
            Token::Delim('-') => -1.0,
            _ => return Err(p.new_custom_error(())),
        };
        let term = parse_product(p, units)?;
        total.length += sign * term.length;
        total.percentage += sign * term.percentage;
    }
    Ok(total)
}

fn parse_product<'i>(p: &mut Parser<'i, '_>, units: Units) -> Result<LengthPercentage, Fail<'i>> {
    let mut value = parse_factor(p, units)?;
    loop {
        let state = p.state();
        match p.next() {
            Ok(Token::Delim('*')) => {
                let factor = parse_factor(p, units)?;
                value = scale(value, factor, true, p)?;
            }
            Ok(Token::Delim('/')) => {
                let factor = parse_factor(p, units)?;
                value = scale(value, factor, false, p)?;
            }
            _ => {
                p.reset(&state);
                return Ok(value);
            }
        }
    }
}

/// `value` times or divided by `factor`, one of the two being a bare number
/// (a bare number is carried as a length with no unit, `percentage` of `NaN`
/// marking it, so `2 * 10px` and `10px * 2` both work).
fn scale<'i>(
    value: LengthPercentage,
    factor: LengthPercentage,
    multiply: bool,
    p: &Parser<'i, '_>,
) -> Result<LengthPercentage, Fail<'i>> {
    let is_number = |v: &LengthPercentage| v.percentage.is_nan();
    match (is_number(&value), is_number(&factor)) {
        (_, true) => {
            let k = if multiply {
                factor.length
            } else {
                1.0 / factor.length
            };
            Ok(LengthPercentage {
                length: value.length * k,
                percentage: value.percentage * k,
            })
        }
        (true, false) if multiply => Ok(LengthPercentage {
            length: factor.length * value.length,
            percentage: factor.percentage * value.length,
        }),
        _ => Err(p.new_custom_error(())),
    }
}

fn parse_factor<'i>(p: &mut Parser<'i, '_>, units: Units) -> Result<LengthPercentage, Fail<'i>> {
    let token = p.next()?.clone();
    match token {
        Token::Number { value, .. } => Ok(LengthPercentage {
            length: value,
            percentage: f32::NAN,
        }),
        Token::ParenthesisBlock => p.parse_nested_block(|inner| parse_sum(inner, units)),
        other => leaf(&other, units).ok_or_else(|| p.new_custom_error(())),
    }
}

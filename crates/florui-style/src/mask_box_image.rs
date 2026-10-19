//! `-webkit-mask-box-image`: a nine-slice image used as a mask, which takes the
//! grammar of `border-image`. The engine's servo build knows the properties of
//! `border-image` but not these, so each part is carried as a custom property
//! and handed to the engine again, as the `border-image` part it is the twin
//! of, on a throwaway element: what comes back is the same [`BorderImage`] a
//! border would have, parsed and computed by the engine itself.

use std::cell::RefCell;
use std::collections::HashMap;

use florui::Element;

use crate::cascade::{BorderImage, Viewport};
use crate::color::Rgba;
use crate::interaction::InteractionState;

/// What `-webkit-mask-box-image` came to on an element: its parts as written, in
/// the order of the longhands (source, slice, width, outset, repeat; `None` for a
/// part the element's rules do not declare), and what lengths in them are
/// relative to. Turned into a [`BorderImage`] by [`resolve_mask_box_image`], which
/// cannot run while styles are being computed.
#[derive(Debug, Clone, PartialEq)]
pub struct MaskBoxImageSpec {
    pub parts: [Option<String>; 5],
    pub font_size: f32,
    pub color: Rgba,
    pub viewport: (f32, f32),
}

/// Each part's initial value, which differ from `border-image`'s own.
const INITIAL: [(&str, &str); 5] = [
    ("border-image-source", "none"),
    ("border-image-slice", "0 fill"),
    ("border-image-width", "auto"),
    ("border-image-outset", "0"),
    ("border-image-repeat", "stretch"),
];

const KEPT: usize = 64;

thread_local! {
    static RESOLVED: RefCell<HashMap<String, Option<Box<BorderImage>>>> =
        RefCell::new(HashMap::new());
}

/// The border image the parts make, in the context of an element with this
/// font size and color in a viewport of this size; `None` when there is no
/// source.
pub fn resolve_mask_box_image(spec: &MaskBoxImageSpec) -> Option<Box<BorderImage>> {
    spec.parts[0].as_ref()?;
    let key = format!("{spec:?}");
    if let Some(found) = RESOLVED.with(|cache| cache.borrow().get(&key).cloned()) {
        return found;
    }
    let image = compute(spec);
    RESOLVED.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= KEPT {
            cache.clear();
        }
        cache.insert(key, image.clone());
    });
    image
}

fn compute(spec: &MaskBoxImageSpec) -> Option<Box<BorderImage>> {
    let MaskBoxImageSpec {
        parts,
        font_size,
        color,
        viewport,
    } = spec;
    // The initial value first, so a value the engine finds invalid leaves it,
    // as an invalid declaration leaves the property where it was.
    let mut declarations = String::new();
    for ((property, initial), value) in INITIAL.iter().zip(parts.iter()) {
        declarations.push_str(&format!("{property}: {initial}; "));
        if let Some(value) = value {
            declarations.push_str(&format!("{property}: {value}; "));
        }
    }
    let css = format!(
        ".m {{ font-size: {font_size}px; color: rgba({}, {}, {}, {}); {declarations} }}",
        color.r,
        color.g,
        color.b,
        f32::from(color.a) / 255.0
    );
    let rules = crate::stylesheet_parse::parse_stylesheet(&css).ok()?;
    let element = Element::node(
        "div",
        vec![("class".to_owned(), "m".to_owned())],
        Vec::new(),
    );
    let arena = crate::tree::Arena::build(&element);
    let styles = crate::cascade::compute(
        &arena,
        &rules,
        &InteractionState::new(),
        Viewport {
            width: viewport.0,
            height: viewport.1,
        },
        &mut crate::animation::AnimationTimeline::default(),
    );
    let root = *arena.roots().first()?;
    styles.get(&root)?.border_image.clone()
}

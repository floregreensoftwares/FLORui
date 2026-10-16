//! Which OpenType features a font has, read from its `GSUB` table: a font that
//! has `smcp` gets its own small capitals, not the scaled ones made here.

use skrifa::FontRef;
use skrifa::raw::{TableProvider, types::Tag};

/// Whether the font at `index` of `data` lists the feature `tag`.
pub(crate) fn font_has_feature(data: &[u8], index: u32, tag: [u8; 4]) -> bool {
    let Ok(font) = FontRef::from_index(data, index) else {
        return false;
    };
    let Ok(gsub) = font.gsub() else {
        return false;
    };
    let Ok(list) = gsub.feature_list() else {
        return false;
    };
    let wanted = Tag::new(&tag);
    list.feature_records()
        .iter()
        .any(|record| record.feature_tag() == wanted)
}

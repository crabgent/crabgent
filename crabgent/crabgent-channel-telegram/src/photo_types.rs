//! Telegram photo-size helpers.

use serde::Deserialize;

/// Photo variant as returned by Telegram `getUpdates` for image messages.
#[derive(Debug, Clone, Deserialize)]
pub struct PhotoSize {
    pub file_id: String,
    #[serde(default)]
    pub width: u64,
    #[serde(default)]
    pub height: u64,
}

/// Select the largest photo variant by width × height.
pub fn select_best_photo_size(sizes: &[PhotoSize]) -> Option<&PhotoSize> {
    sizes
        .iter()
        .max_by_key(|size| size.width.saturating_mul(size.height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_best_photo_size_picks_largest() {
        let sizes = vec![
            PhotoSize {
                file_id: "small".into(),
                width: 2,
                height: 2,
            },
            PhotoSize {
                file_id: "large".into(),
                width: 4,
                height: 5,
            },
            PhotoSize {
                file_id: "medium".into(),
                width: 3,
                height: 3,
            },
        ];
        let best = select_best_photo_size(&sizes).expect("best photo");
        assert_eq!(best.file_id, "large");
    }
}

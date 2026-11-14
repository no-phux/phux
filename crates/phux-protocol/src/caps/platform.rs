/// The device a client says it runs on (`HELLO` field `PLATFORM`).
///
/// One short lowercase word: 1 to 16 bytes of `[a-z0-9-]`. The vocabulary is
/// a convention, not a closed set: `macos`, `linux`, `windows`, `ios`,
/// `ipados`, `android`, `web`. It is whatever the client reported, a label
/// and never an authentication fact; anything else decodes as absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientPlatform {
    len: u8,
    bytes: [u8; Self::MAX_LEN],
}

impl ClientPlatform {
    /// The longest word, in bytes.
    pub const MAX_LEN: usize = 16;

    /// `word` as a platform, or `None` when it is empty, too long, or holds
    /// a byte outside `[a-z0-9-]`.
    #[must_use]
    pub const fn parse(word: &str) -> Option<Self> {
        let src = word.as_bytes();
        if src.is_empty() || src.len() > Self::MAX_LEN {
            return None;
        }
        let mut bytes = [0u8; Self::MAX_LEN];
        let mut i = 0;
        while i < src.len() {
            if !matches!(src[i], b'a'..=b'z' | b'0'..=b'9' | b'-') {
                return None;
            }
            bytes[i] = src[i];
            i += 1;
        }
        #[allow(clippy::cast_possible_truncation, reason = "at most MAX_LEN")]
        let len = src.len() as u8;
        Some(Self { len, bytes })
    }

    /// The platform this build runs on: `web` in a browser, else the
    /// target OS word (`macos`, `linux`, `windows`, `ios`, `android`, ...).
    #[must_use]
    pub const fn host() -> Option<Self> {
        Self::parse(if cfg!(target_family = "wasm") {
            "web"
        } else {
            std::env::consts::OS
        })
    }

    /// The word.
    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or_default()
    }
}

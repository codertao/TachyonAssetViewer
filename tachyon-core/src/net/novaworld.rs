//! NovaWorld join tokens. A stock client joins a specific host only through the NovaWorld
//! browser: when a page's `<title>` contains `NK=<enc>&CK=<enc>&`, the game decodes `NK` to
//! `host:port` and `CK` to the game id and starts the join (`NW_OnPageLoaded 0x44d860`,
//! `NW_DecodeJoinTitle 0x44d6f0`; `multiplayer.md` "Browsing and joining").

const KEY: &[u8] = b"cfhdcegjigecjehcgjdhe";

/// Output char i = input char - key[i] + '0', so input = output + key[i] - '0'. Only the
/// first 21 characters have a key byte, so longer strings can't be encoded.
pub fn encode(s: &str) -> Option<String> {
    if s.len() > KEY.len() || !s.is_ascii() {
        return None;
    }
    s.bytes().zip(KEY).map(|(c, &k)| {
        let e = c as i32 + k as i32 - b'0' as i32;
        (33..127).contains(&e).then_some(e as u8 as char).filter(|&ch| ch != '&' && ch != '<' && ch != '>')
    }).collect()
}

pub fn decode(s: &str) -> String {
    s.bytes().zip(KEY).map(|(c, &k)| (c as i32 - k as i32 + b'0' as i32) as u8 as char).collect()
}

/// The `<title>` token for `host:port` and a game id.
pub fn join_title(host_port: &str, game_id: u32) -> Option<String> {
    Some(format!("NK={}&CK={}&", encode(host_port)?, encode(&game_id.to_string())?))
}

/// A replacement for the game's `Startup.htm`: opening the Network browser joins `host_port`.
pub fn join_page(host_port: &str, game_id: u32, label: &str) -> Option<String> {
    let title = join_title(host_port, game_id)?;
    let label: String = label.chars().filter(|c| c.is_ascii_alphanumeric() || " -_.:".contains(*c)).collect();
    Some(format!(
        "<HTML>\r\n<HEAD>\r\n<TITLE>{title}</TITLE>\r\n</HEAD>\r\n<BODY TEXT=\"#C0C0C0\" BGCOLOR=\"#000000\">\r\n\
<CENTER><P><B><FONT SIZE=+2>Joining {label} at {host_port}...</FONT></B></P></CENTER>\r\n</BODY>\r\n</HTML>\r\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for s in ["127.0.0.1:2766", "192.168.100.200:27660", "1000", "9999"] {
            assert_eq!(decode(&encode(s).unwrap()), s);
        }
        assert!(encode("192.168.100.200:276600").is_none());
        let t = join_title("10.0.0.5:2766", 1234).unwrap();
        assert!(t.starts_with("NK=") && t.ends_with('&') && t.matches('&').count() == 2);
    }
}

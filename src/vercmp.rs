use std::cmp::Ordering;

fn rpmvercmp(a: &[u8], b: &[u8]) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (mut one, mut two) = (0usize, 0usize);
    let (mut p1, mut p2) = (0usize, 0usize);
    let at = |s: &[u8], i: usize| s.get(i).copied().unwrap_or(0);
    while at(a, one) != 0 && at(b, two) != 0 {
        while at(a, one) != 0 && !at(a, one).is_ascii_alphanumeric() {
            one += 1;
        }
        while at(b, two) != 0 && !at(b, two).is_ascii_alphanumeric() {
            two += 1;
        }
        if at(a, one) == 0 || at(b, two) == 0 {
            break;
        }
        if one - p1 != two - p2 {
            return (one - p1).cmp(&(two - p2));
        }
        p1 = one;
        p2 = two;
        let isnum = at(a, p1).is_ascii_digit();
        if isnum {
            while at(a, p1).is_ascii_digit() {
                p1 += 1;
            }
            while at(b, p2).is_ascii_digit() {
                p2 += 1;
            }
        } else {
            while at(a, p1).is_ascii_alphabetic() {
                p1 += 1;
            }
            while at(b, p2).is_ascii_alphabetic() {
                p2 += 1;
            }
        }
        if two == p2 {
            return if isnum {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
        let mut s1 = &a[one..p1];
        let mut s2 = &b[two..p2];
        if isnum {
            while s1.first() == Some(&b'0') {
                s1 = &s1[1..];
            }
            while s2.first() == Some(&b'0') {
                s2 = &s2[1..];
            }
            match s1.len().cmp(&s2.len()) {
                Ordering::Equal => {}
                o => return o,
            }
        }
        match s1.cmp(s2) {
            Ordering::Equal => {}
            o => return o,
        }
        one = p1;
        two = p2;
    }
    let (c1, c2) = (at(a, one), at(b, two));
    if c1 == 0 && c2 == 0 {
        return Ordering::Equal;
    }
    if (c1 == 0 && !c2.is_ascii_alphabetic()) || c1.is_ascii_alphabetic() {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

fn parse_evr(evr: &str) -> (&str, &str, Option<&str>) {
    let digits = evr.bytes().take_while(u8::is_ascii_digit).count();
    let (epoch, version) = match evr[digits..].strip_prefix(':') {
        Some(v) if digits > 0 => (&evr[..digits], v),
        Some(v) => ("0", v),
        None => ("0", evr),
    };
    match version.rfind('-') {
        Some(i) => (epoch, &version[..i], Some(&version[i + 1..])),
        None => (epoch, version, None),
    }
}

pub fn vercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (e1, v1, r1) = parse_evr(a);
    let (e2, v2, r2) = parse_evr(b);
    let mut ret = rpmvercmp(e1.as_bytes(), e2.as_bytes());
    if ret == Ordering::Equal {
        ret = rpmvercmp(v1.as_bytes(), v2.as_bytes());
        if ret == Ordering::Equal {
            if let (Some(r1), Some(r2)) = (r1, r2) {
                ret = rpmvercmp(r1.as_bytes(), r2.as_bytes());
            }
        }
    }
    ret
}

#[cfg(test)]
mod tests {
    use super::*;
    use Ordering::*;

    fn check(a: &str, b: &str, want: Ordering) {
        assert_eq!(vercmp(a, b), want, "{a} vs {b}");
        assert_eq!(vercmp(b, a), want.reverse(), "{b} vs {a}");
    }

    #[test]
    fn pacman_vercmp_suite() {
        check("1.5.0", "1.5.0", Equal);
        check("1.5.1", "1.5.0", Greater);
        check("1.5.1", "1.5", Greater);
        check("1.5.0-1", "1.5.0-1", Equal);
        check("1.5.0-1", "1.5.0-2", Less);
        check("1.5.0-1", "1.5.1-1", Less);
        check("1.5.0-2", "1.5.1-1", Less);
        check("1.5-1", "1.5", Equal);
        check("1.5.a", "1.5", Greater);
        check("1.5.b", "1.5.a", Greater);
        check("1.5.1", "1.5.b", Greater);
        check("1.5.b-1", "1.5.b", Equal);
        check("1.5-1", "1.5.b", Less);
        check("1.5pre1", "1.5", Less);
        check("1.5pre1", "1.5pre2", Less);
        check("1.5rc1", "1.5pre1", Greater);
        check("1.5", "1.5a", Greater);
        check("1.0", "1.0.0", Less);
        check("1..0", "1.0", Greater);
        check("1.1", "1.1.0", Less);
        check("1.0.01", "1.0.1", Equal);
        check("10", "9", Greater);
        check("1.0", "1_0", Equal);
        check("0:1.0", "0:1.0", Equal);
        check("0:1.0", "0:1.1", Less);
        check("1:1.0", "0:1.0", Greater);
        check("1:1.0", "0:1.1", Greater);
        check("1:1.0", "2:1.1", Less);
        check("0:1.0", "1.0", Equal);
        check("1:1.0", "1.0", Greater);
        check("1:1.0", "1.1", Greater);
        check("1.1", "1:1.0", Less);
        check("1.0-1", "1:0.5-1", Less);
        check("2.0.0-1.1", "2.0.0-1", Greater);
        check("1.2.3+r4.gabc-1", "1.2.3-1", Greater);
    }

    #[test]
    fn evr_parsing() {
        assert_eq!(parse_evr("1:2.3-4"), ("1", "2.3", Some("4")));
        assert_eq!(parse_evr("2.3-4"), ("0", "2.3", Some("4")));
        assert_eq!(parse_evr("2.3"), ("0", "2.3", None));
        assert_eq!(parse_evr(":2.3"), ("0", "2.3", None));
        assert_eq!(parse_evr("a-b-c"), ("0", "a-b", Some("c")));
    }

    #[test]
    fn hostile_input_does_not_panic() {
        for s in ["", "-", ":", "::", "-:-", "\u{e9}1", "1\u{0}2", "...", "1-"] {
            let _ = vercmp(s, "1.0");
            let _ = vercmp("1.0", s);
            let _ = vercmp(s, s);
        }
    }
}

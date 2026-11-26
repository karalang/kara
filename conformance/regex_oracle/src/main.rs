// Oracle for conformance/regex.py: reads cases (hex pattern, hex text, hex
// rep per line, tab-separated) and prints what docs/library/regex.md says
// each std.regex method returns, from the Rust regex crate.
use regex::Regex;
use std::io::BufRead;

fn unhex(s: &str) -> String {
    let b: Vec<u8> = (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect();
    String::from_utf8(b).unwrap()
}
fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}
fn span(m: Option<regex::Match>) -> String {
    match m {
        Some(m) => format!("{}-{}:{}", m.start(), m.end(), hex(m.as_str())),
        None => "none".into(),
    }
}

fn main() {
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let f: Vec<&str> = line.split('\t').collect();
        let (p, t, r) = (unhex(f[0]), unhex(f[1]), unhex(f[2]));
        // `Regex.escape(t)`, and the escaped pattern finding `t` whole.
        let esc = regex::escape(&t);
        let whole = Regex::new(&esc).unwrap().find(&t).map(|m| (m.start(), m.end()));
        println!("X {} {}", hex(&esc), whole == Some((0, t.len())));
        let re = match Regex::new(&p) {
            Ok(re) => re,
            Err(_) => {
                println!("C err");
                continue;
            }
        };
        println!("C ok");
        println!("M {}", re.is_match(&t));
        println!("F {}", span(re.find(&t)));
        let all: Vec<String> = re.find_iter(&t).map(|m| span(Some(m))).collect();
        println!("A {}", all.join(","));
        let sp: Vec<String> = re.split(&t).map(|s| hex(s)).collect();
        println!("S {}", sp.join(","));
        println!("R {}", hex(&re.replace(&t, r.as_str())));
        println!("RA {}", hex(&re.replace_all(&t, r.as_str())));
        let caps = |c: &regex::Captures| {
            let mut out = vec![format!("{}", c.len())];
            for i in -1..=(c.len() as i64) {
                out.push(if i < 0 { "none".into() } else { span(c.get(i as usize)) });
            }
            for n in ["n1", "n2", "zz"] {
                out.push(span(c.name(n)));
            }
            out.join(" ")
        };
        match re.captures(&t) {
            Some(c) => println!("K {}", caps(&c)),
            None => println!("K none"),
        }
        for c in re.captures_iter(&t) {
            println!("KA {}", caps(&c));
        }
        println!("E");
    }
}

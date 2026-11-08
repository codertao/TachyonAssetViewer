//! Dump a CBIN file as text: `cargo run --release --example cbindump -- FILE...`
#[path = "../src/cbin.rs"]
#[allow(dead_code)]
mod cbin;

fn main() {
    for f in std::env::args().skip(1) {
        let d = std::fs::read(&f).expect("read");
        match cbin::parse(&d) {
            Ok(groups) => {
                println!("== {f}");
                for g in groups {
                    println!("[{}]", g.name);
                    for i in g.items {
                        println!("  {}: {:?}", i.key, i.values);
                    }
                }
            }
            Err(e) => println!("== {f}: {e}"),
        }
    }
}

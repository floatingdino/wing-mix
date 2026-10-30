//! Wing Mix headless CLI (analyse, match, dry-run against the simulator).
//!
//! Scaffold only (M001).

fn main() {
    println!("wing-mix-cli {}", env!("CARGO_PKG_VERSION"));
}

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold() {
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
    }
}

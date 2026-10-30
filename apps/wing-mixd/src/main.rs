//! Wing Mix daemon (capture, analysis, engine, control socket).
//!
//! Scaffold only (M001).

fn main() {
    println!("wing-mixd {}", env!("CARGO_PKG_VERSION"));
}

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold() {
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
    }
}

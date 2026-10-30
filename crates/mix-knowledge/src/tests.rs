use super::*;

#[test]
fn embedded_is_valid() {
    let kb = KnowledgeBase::parse_unchecked(&EMBEDDED).unwrap();
    let p = check::check(&kb);
    assert!(p.is_empty(), "{}", p.join("\n"));
}

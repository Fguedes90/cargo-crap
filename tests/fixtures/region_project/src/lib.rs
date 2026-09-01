pub fn one_line_match(x: u8) -> u8 { match x { 0 => 10, 1 => 20, 2 => 30, _ => 40 } }

#[cfg(test)]
mod tests {
    #[test]
    fn only_zero() {
        assert_eq!(super::one_line_match(0), 10);
    }
}

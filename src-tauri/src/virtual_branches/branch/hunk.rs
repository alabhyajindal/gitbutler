use std::{fmt::Display, ops::RangeInclusive};

use anyhow::{anyhow, Context, Result};

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Hunk {
    start: usize,
    end: usize,
}

impl From<RangeInclusive<usize>> for Hunk {
    fn from(range: RangeInclusive<usize>) -> Self {
        Hunk {
            start: *range.start(),
            end: *range.end(),
        }
    }
}

impl TryFrom<&str> for Hunk {
    type Error = anyhow::Error;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        let mut range = s.split('-');
        let start = if let Some(raw_start) = range.next() {
            raw_start
                .parse::<usize>()
                .context(format!("failed to parse start of range: {}", s))
        } else {
            Err(anyhow!("invalid range: {}", s))
        }?;

        let end = if let Some(raw_end) = range.next() {
            raw_end
                .parse::<usize>()
                .context(format!("failed to parse end of range: {}", s))
        } else {
            Err(anyhow!("invalid range: {}", s))
        }?;
        let hunk = Hunk::new(start, end)?;

        Ok(hunk)
    }
}

impl Display for Hunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.start, self.end)
    }
}

impl Hunk {
    pub fn new(start: usize, end: usize) -> Result<Self> {
        if start > end {
            Err(anyhow!("invalid range: {}-{}", start, end))
        } else {
            Ok(Hunk { start, end })
        }
    }

    pub fn contains(&self, line: &usize) -> bool {
        self.start <= *line && self.end >= *line
    }

    pub fn intersects(&self, another: &Hunk) -> bool {
        self.contains(&another.start)
            || self.contains(&another.end)
            || another.contains(&self.start)
            || another.contains(&self.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_from_string() {
        let hunk = Hunk::try_from("1-2").unwrap();
        assert_eq!("1-2", hunk.to_string());
    }

    #[test]
    fn parse_invalid() {
        assert!(Hunk::try_from("3-2-garbage").is_err());
    }

    #[test]
    fn parse_invalid_2() {
        assert!(Hunk::try_from("3-2").is_err());
    }
}

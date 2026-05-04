// design_studies/money_type/money.rs
//
// Domain type with behavior — Rust variant.
// See design_studies/money_type/findings.md for cross-language notes.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Currency {
    USD,
    EUR,
    GBP,
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            Currency::USD => "USD",
            Currency::EUR => "EUR",
            Currency::GBP => "GBP",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Money {
    pub amount: i64,         // minor units (cents)
    pub currency: Currency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MismatchedCurrency {
    pub left: Currency,
    pub right: Currency,
}

impl fmt::Display for MismatchedCurrency {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "currency mismatch: {} vs {}", self.left, self.right)
    }
}

impl std::error::Error for MismatchedCurrency {}

impl Money {
    pub fn new(amount: i64, currency: Currency) -> Self {
        Money { amount, currency }
    }

    pub fn add(self, other: Money) -> Result<Money, MismatchedCurrency> {
        if self.currency != other.currency {
            return Err(MismatchedCurrency {
                left: self.currency,
                right: other.currency,
            });
        }
        Ok(Money::new(self.amount + other.amount, self.currency))
    }

    // Same-currency comparison only. We deliberately don't derive Ord
    // because lex-comparing (amount, currency) would silently treat
    // USD < EUR < GBP, which is meaningless.
    pub fn cmp_same(self, other: Money) -> Result<std::cmp::Ordering, MismatchedCurrency> {
        if self.currency != other.currency {
            return Err(MismatchedCurrency {
                left: self.currency,
                right: other.currency,
            });
        }
        Ok(self.amount.cmp(&other.amount))
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let whole = self.amount / 100;
        let cents = (self.amount % 100).abs();
        write!(f, "{whole}.{cents:02} {}", self.currency)
    }
}

fn main() {
    let a = Money::new(1234, Currency::USD);
    let b = Money::new(567, Currency::USD);
    let sum = a.add(b).unwrap();
    println!("{a} + {b} = {sum}");
}

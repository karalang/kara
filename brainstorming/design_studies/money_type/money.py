# design_studies/money_type/money.py
#
# Domain type with behavior — Python variant.
# See design_studies/money_type/findings.md for cross-language notes.

from dataclasses import dataclass
from enum import Enum
from functools import total_ordering


class Currency(Enum):
    USD = "USD"
    EUR = "EUR"
    GBP = "GBP"


@total_ordering
@dataclass(frozen=True)
class Money:
    amount: int              # minor units (cents)
    currency: Currency

    def add(self, other: "Money") -> "Money":
        if self.currency != other.currency:
            raise ValueError(
                f"currency mismatch: {self.currency.value} + {other.currency.value}"
            )
        return Money(self.amount + other.amount, self.currency)

    def __lt__(self, other: "Money") -> bool:
        if self.currency != other.currency:
            raise ValueError(
                f"cannot compare {self.currency.value} and {other.currency.value}"
            )
        return self.amount < other.amount

    def __str__(self) -> str:
        whole = self.amount // 100
        cents = abs(self.amount % 100)
        return f"{whole}.{cents:02d} {self.currency.value}"


def main() -> None:
    a = Money(1234, Currency.USD)
    b = Money(567, Currency.USD)
    total = a.add(b)
    print(f"{a} + {b} = {total}")


if __name__ == "__main__":
    main()

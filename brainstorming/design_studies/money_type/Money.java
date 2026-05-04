// design_studies/money_type/Money.java
//
// Domain type with behavior. First design-study brick to exercise
// the bundled vs. separated `impl` question (v44 / v45 D1) honestly —
// Money has fields, methods, and trait conformance, with no injection
// or effect-resource machinery in sight.
//
// Usage: java Money

public final class Money implements Comparable<Money> {

    public enum Currency { USD, EUR, GBP }

    public final long amount;            // minor units (cents)
    public final Currency currency;

    public Money(long amount, Currency currency) {
        this.amount = amount;
        this.currency = currency;
    }

    public Money add(Money other) {
        if (this.currency != other.currency) {
            throw new IllegalArgumentException(
                "currency mismatch: " + this.currency + " + " + other.currency);
        }
        return new Money(this.amount + other.amount, this.currency);
    }

    @Override
    public int compareTo(Money other) {
        if (this.currency != other.currency) {
            throw new IllegalArgumentException(
                "cannot compare " + this.currency + " and " + other.currency);
        }
        return Long.compare(this.amount, other.amount);
    }

    @Override
    public boolean equals(Object o) {
        if (!(o instanceof Money)) return false;
        Money m = (Money) o;
        return this.amount == m.amount && this.currency == m.currency;
    }

    @Override
    public int hashCode() {
        return Long.hashCode(amount) * 31 + currency.hashCode();
    }

    @Override
    public String toString() {
        long whole = amount / 100;
        long cents = Math.abs(amount % 100);
        return String.format("%d.%02d %s", whole, cents, currency);
    }

    public static void main(String[] args) {
        Money a = new Money(1234, Currency.USD);
        Money b = new Money(567, Currency.USD);
        Money sum = a.add(b);
        System.out.println(a + " + " + b + " = " + sum);
    }
}

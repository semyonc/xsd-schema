# Implementation Limits

Where the specifications leave a value space unbounded and this crate stores
values in a bounded type, and what happens at the bound. Datatypes 1.1 §5.4
allows such limits on two conditions: a value beyond them "must not be treated
as invalid", and it "must not be quietly changed to a supported value".

## 1. `xs:decimal`

### Validation is exact

The value space of `xs:decimal` is unbounded, and every value is validated
exactly: `totalDigits`, `fractionDigits`, the bound facets, `enumeration`,
fixed/default values and identity-constraint fields are decided on the exact
value, however large or precise. The same holds for the integer types derived
from `xs:decimal`.

### The typed value

The typed value (`XmlAtomicValue::Decimal`, a `rust_decimal::Decimal`) holds a
value exactly when it is `m / 10^s` for integers with

- `|m| ≤ 2^96 − 1 = 79228162514264337593543950335`, and
- `0 ≤ s ≤ 28`

(trailing fraction zeros do not count). Every value that a `totalDigits` facet
of 28 accepts qualifies: Datatypes §4.3.11.3 makes it `i / 10^n` with
`|i| < 10^28 < 2^96` and `n ≤ 28`. A count of significant digits does not
decide it — `10^−29` and `10^29` have one each and do not qualify.

A valid value beyond that is never rounded:

- its typed value is its canonical representation (Datatypes §3.3.3.2), an
  `XmlAtomicValue::String` with the `xs:decimal` type code, so
  `XmlValue::as_decimal` returns `None`;
- the validator reports a warning with code `xs-decimal-capacity` (Datatypes 1.1
  §5.4) while `ValidationFlags::REPORT_WARNINGS` is set, which it is by default.

### In XPath

A decimal held in canonical form:

- compares exactly;
- casts to `xs:string` (the canonical form), `xs:boolean`, `xs:integer`
  (truncation), `xs:double` and `xs:float` (nearest value);
- raises `FOAR0002` in arithmetic, rather than computing with an approximation.

An XSD 1.1 assertion (`xs:assert`, `xs:assertion`) whose evaluation fails only
because it computed with such a value is not counted as false: its outcome is
unknown, and the value carries the `xs-decimal-capacity` warning. Any other
error still fails the assertion (Structures §3.13.4.1), also when such a value
occurs elsewhere in the instance.

Casting a string to `xs:decimal`, or writing a decimal literal, follows F&O
§17.1.1 and §6.2:

| Value | Result |
| --- | --- |
| magnitude larger than held | `FOAR0002` |
| nonzero magnitude below `10^−28`, the smallest positive value held | `0` (underflow) |
| any other value with more precision than is held | `FOCA0006` |

## 2. `xs:integer`

`xs:integer` values have no size limit (`BigInt`).

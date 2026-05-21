# Feature: Roman numeral conversion

## Goal

Convert a positive integer in the range `1 .. 3999` (inclusive) to its
standard Roman numeral representation.

## API

- Module path: `df_demo3.roman`
- Function:
  ```python
  def to_roman(n: int) -> str: ...
  ```

## Examples

| `n`  | `to_roman(n)` |
|------|---------------|
| 1    | `I`           |
| 4    | `IV`          |
| 9    | `IX`          |
| 14   | `XIV`         |
| 40   | `XL`          |
| 90   | `XC`          |
| 400  | `CD`          |
| 900  | `CM`          |
| 1994 | `MCMXCIV`     |
| 3999 | `MMMCMXCIX`   |

## Required behaviour

1. `to_roman` returns the canonical Roman numeral string (uppercase ASCII).
2. Use subtractive notation where standard: `IV` not `IIII`, `IX` not `VIIII`,
   `XL` not `XXXX`, `XC` not `LXXXX`, `CD` not `CCCC`, `CM` not `DCCCC`.
3. Inputs outside `1 .. 3999` are out of scope (you do not need to validate).

## Non-Goals

- CLI wrapper
- Input validation
- Reverse conversion (`from_roman`)
- Internationalisation

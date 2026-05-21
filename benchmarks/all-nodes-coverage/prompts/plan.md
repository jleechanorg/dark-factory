# Plan: ${goal}

You will implement a small feature in this repository. The full feature spec
is reproduced below — work from this text only, do not look for the spec
elsewhere on disk.

## Feature spec

### Goal

Convert a positive integer in the range `1 .. 3999` (inclusive) to its
standard Roman numeral representation.

### API

- Module path: `df_demo3.roman`
- Function: `def to_roman(n: int) -> str: ...`

### Examples

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

### Required behaviour

1. `to_roman` returns the canonical Roman numeral string (uppercase ASCII).
2. Use subtractive notation where standard (IV, IX, XL, XC, CD, CM).
3. Inputs outside `1 .. 3999` are out of scope.

### Non-Goals

- CLI wrapper, input validation, reverse conversion, internationalisation.

## Your task right now

Write a 2-3 sentence plan describing the file(s) you will create and what
their contents will be. Do NOT write code yet. Output the plan as plain
text, then stop.

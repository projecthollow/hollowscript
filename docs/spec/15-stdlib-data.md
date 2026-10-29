# 15. Standard library: JSON and time

Both are pure. The conventions of [section 13.1](13-stdlib-core.md#131-conventions) apply. `json` and `time` are prelude namespaces: records whose fields are pure functions, called as `json.parse(text)`.

## 15.1 JSON

JSON text is represented by values of the prelude enum `Json`:

```
enum Json {
    Null,
    Bool(Bool),
    Int(Int),
    Float(Float),
    String(String),
    Array([Json]),
    Object(Map<String, Json>),
}
```

A variant is written through the enum name (`Json.Int(3)`, `Json.Null`) and matched without it (`Int(n)`, `Null`). `Json` values are equatable and are ordinary enum values in every other respect.

`JsonError` is the prelude record type `{ message: String, offset: Int }`. `offset` is the byte offset in the input at which the problem was detected.

| Signature | Meaning | Cost |
|---|---|---|
| `json.parse(text: String): Result<Json, JsonError>` | parses `text` | n (bytes of `text`) |
| `json.stringify(value: Json): String` | compact text; faults HS1011 if the value nests more than 128 deep | n (bytes of the result) |
| `json.pretty(value: Json, indent: Int): String` | indented text; faults HS1011 if `indent` is below 1 or above 16, or if the value nests more than 128 deep | n (bytes of the result) |
| `json.field(value: Json, name: String): Json?` | if `value` is an `Object` with the key `name`, its value; otherwise `none` | 1 |
| `json.at(value: Json, index: Int): Json?` | if `value` is an `Array` and `index` is in range, the element; otherwise `none` | 1 |

**Parsing.** The grammar is that of RFC 8259 and is strict: no comments, no trailing commas, no single quotes, no leading zeros, no `NaN` or `Infinity`. Whitespace between tokens is space, tab, LF and CR only. The whole text must be exactly one value, optionally surrounded by whitespace. A byte order mark at the start is an error.

- A number is `Int` if it has no fraction and no exponent and fits in an `Int` (`-0` is `Int(0)`), and `Float` otherwise, including an integer too large for an `Int`. A `Float` is the nearest binary64 value, ties to the even significand; a number that would be infinite is an error. The exponent is handled as for `parseFloat` ([section 13.4](13-stdlib-core.md#134-converting-between-numbers-and-text)). A `Float` result of negative zero is `0.0`.
- Strings support the escapes `\"`, `\\`, `\/`, `\b`, `\f`, `\n`, `\r`, `\t` and `\uXXXX`. A `\u` escape that is a high surrogate must be followed by a `\u` escape that is a low surrogate, and the pair is one code point; an unpaired surrogate escape is an error. A raw control character (below U+0020) inside a string is an error.
- If an object has the same key twice, the later value wins.
- Arrays and objects may be nested at most 128 deep.

`json.parse` returns `Err` with one of these messages, chosen by the first problem found scanning left to right, and with the `offset` given for it:

| Message | When | `offset` |
|---|---|---|
| `unexpected end of input` | the input ends before the value is complete: inside an array, an object, a string or an escape, after a `,` or a `:`, or inside a literal (a number is scanned as one token, see `invalid number`) | the length of the input |
| `unexpected character` | a byte that cannot start or continue the current production outside a string and a number: a byte that cannot start a value, an unquoted key, a missing `:` or `,`, a `,` before `]` or `}`, a byte order mark, and the first byte that differs from `true`, `false` or `null` when a value starts with `t`, `f` or `n` | that byte (0 for a byte order mark) |
| `invalid escape sequence` | a `\` followed by a byte that is not one of the escape characters | the backslash |
| `invalid unicode escape` | `\u` followed by a byte that is not a hexadecimal digit before four have been read, or a high surrogate escape not followed by a low surrogate escape, or a low surrogate escape on its own | the backslash of the escape (of the first escape of a pair) |
| `invalid string` | a raw byte below U+0020 inside a string | that byte |
| `invalid number` | a malformed number: `-` with no digit after it, a `0` followed by a digit (`01`), a `.` with no digit after it (`1.`), an `e` or `E` with no digit after it and its optional sign (`1e`) | the first byte of the number |
| `number out of range` | a `Float` that would be infinite | the first byte of the number |
| `nesting too deep` | the opening bracket that makes the depth 129, the outermost array or object being depth 1 | that bracket |
| `unexpected trailing characters` | a non-whitespace byte after the value | that byte |

A value that starts with `t`, `f` or `n` is matched byte by byte against `true`, `false` or `null`, and after a complete literal the parse goes on, so `truex` is the value `true` followed by `x`, which is `unexpected trailing characters` at the top level and `unexpected character` inside an array or an object. A number is `-`, then `0` or a digit from 1 to 9 followed by digits, then optionally `.` and one or more digits, then optionally an exponent; `.5` and `+1` are `unexpected character` because a value cannot start with `.` or `+`.

**Text output.** `json.stringify` writes no white space:

- `Null` is `null`, `Bool` is `true` or `false`, `Int` is its decimal digits, `Float` is its text as defined in [section 3.9](03-expressions.md#39-string-interpolation) (which is valid JSON in every case);
- a `String` is written between double quotes; `"` and `\` are escaped as `\"` and `\\`, the control characters U+0008, U+000C, U+000A, U+000D and U+0009 as `\b`, `\f`, `\n`, `\r` and `\t`, every other code point below U+0020, and U+007F, as `\u00xx` with four lower-case hexadecimal digits, and every other code point is written as itself in UTF-8 (`/` is not escaped);
- an `Array` is `[` the elements separated by `,` `]`;
- an `Object` is `{` the members as `"key":value` separated by `,` `}`, in ascending key order.

`json.pretty(value, indent)` writes the same text with a line feed and `indent` spaces per level of nesting before each array element and object member and before the closing bracket, a single space after the `:` of a member, and an empty array or object as `[]` or `{}`. The `,` that separates two elements or members follows the earlier one directly, before the line feed. There is no trailing line feed. The exact length of the text of `stringify` and `pretty` is computed before the text is built and charged ([section 11.4](11-resource-limits.md#114-memory)), and the encoder MUST NOT recurse without bound.

```hollow
uses print

constant doc = Json.Array([Json.Int(1), Json.Object(["a": Json.Null])])
effect print(json.pretty(doc, 2))
```

Output:

```text
[
  1,
  {
    "a": null
  }
]
```

```hollow
uses print

constant parsed = json.parse("{\"b\": [1, 2.5, true], \"a\": null}")
match parsed {
    Ok(value) {
        effect print(json.stringify(value))
    }
    Err(error) {
        effect print("bad json at ${error.offset}: ${error.message}")
    }
}

constant doc = Json.Object(["name": Json.String("Ada"), "age": Json.Int(36)])
effect print(json.stringify(doc))

match json.field(doc, "name") {
    String(name) {
        effect print("hello ${name}")
    }
    else {
        effect print("no name")
    }
}

match json.parse("{") {
    Ok(_) {
        effect print("parsed")
    }
    Err(error) {
        effect print("${error.message} at ${error.offset}")
    }
}
```

Output:

```text
{"a":null,"b":[1,2.5,true]}
{"age":36,"name":"Ada"}
hello Ada
unexpected end of input at 1
```

## 15.2 Time

Time is calendar arithmetic on integers. The library knows only UTC and fixed offsets; there is no time zone database. Reading the current time is the `clock` capability ([section 16.3](16-standard-capabilities.md#163-clock)).

- An **instant** is an `Int`: the number of milliseconds since 1970-01-01T00:00:00.000Z, ignoring leap seconds. The valid range is from 0001-01-01T00:00:00.000Z (-62135596800000) to 9999-12-31T23:59:59.999Z (253402300799999).
- A **date** is an `Int`: the number of days since 1970-01-01 in the proleptic Gregorian calendar. The valid range is from 0001-01-01 (-719162) to 9999-12-31 (2932896).
- `TimeParts` is the prelude record type `{ year: Int, month: Int, day: Int, hour: Int, minute: Int, second: Int, millisecond: Int, weekday: Int }`. `month` is 1 to 12, `day` 1 to the length of the month, `hour` 0 to 23, `minute` and `second` 0 to 59, `millisecond` 0 to 999, and `weekday` is 1 for Monday to 7 for Sunday.

| Signature | Meaning | Cost |
|---|---|---|
| `time.parse(text: String): Result<Int, String>` | parses an RFC 3339 date-time to an instant | n (bytes of `text`) |
| `time.format(instant: Int): String` | the instant as UTC text | 1 |
| `time.parts(instant: Int): TimeParts` | the UTC calendar fields of the instant | 1 |
| `time.fromParts(parts: TimeParts): Result<Int, String>` | the instant for UTC calendar fields | 1 |
| `time.parseDate(text: String): Result<Int, String>` | parses `YYYY-MM-DD` to a date | n (bytes of `text`) |
| `time.formatDate(date: Int): String` | the date as `YYYY-MM-DD` | 1 |

**`time.parse`.** The text is `YYYY-MM-DD`, the letter `T`, `HH:MM:SS`, optionally `.` and 1 to 9 fractional digits, and then either the letter `Z` or an offset `+HH:MM` or `-HH:MM`. All letters are upper case, and there is no other separator and no white space. The fields must be valid (`month` 1 to 12, `day` valid in that month and year, `hour` below 24, `minute` and `second` below 60; a leap second `:60` is not valid, hours of an offset below 24, minutes of an offset below 60). Fractional digits beyond the third are truncated. The result is the UTC instant, that is the local time minus the offset. An offset of `-00:00` is accepted and equals `+00:00`. The result is `Err("invalid timestamp")` if the text does not have that form or a field is not valid, and `Err("timestamp out of range")` if the resulting instant is outside the valid range, which includes a year 0000 in the text, although the fields are otherwise valid.

**`time.format`.** The result is `YYYY-MM-DDTHH:MM:SSZ` when the instant has no milliseconds, and `YYYY-MM-DDTHH:MM:SS.mmmZ` with exactly three digits otherwise (`.500`, not `.5`). It faults with HS1011 if the instant is outside the valid range.

**`time.parts`** faults with HS1011 outside the valid range. It uses floor division, so `time.parts(-1)` is 1969-12-31 at 23:59:59.999. **`time.fromParts`** returns `Err("invalid time parts")` if any field is out of its range or names a day that does not exist (February 30); `weekday` is ignored; the year must be from 1 to 9999, and the result must lie in the valid instant range, otherwise it is also `Err("invalid time parts")`.

**`time.parseDate`** accepts exactly `YYYY-MM-DD` with a valid calendar date and a year from 0001 to 9999, and returns `Err("invalid date")` otherwise (so `0000-01-01` is invalid). **`time.formatDate`** faults with HS1011 outside the valid date range.

Two forms of adding time need no library function: adding milliseconds to an instant is integer addition, and a day is 86,400,000 milliseconds.

```hollow
uses print

match time.parse("2026-07-26T09:15:00+01:00") {
    Ok(instant) {
        effect print(time.format(instant))
        constant p = time.parts(instant)
        effect print("${p.year} ${p.month} ${p.weekday}")
    }
    Err(message) {
        effect print(message)
    }
}

match time.parseDate("2026-13-45") {
    Ok(_) {
        effect print("valid")
    }
    Err(message) {
        effect print(message)
    }
}
```

Output:

```text
2026-07-26T08:15:00Z
2026 7 7
invalid date
```

```hollow
uses print

constant p = time.parts(-1)
effect print("${p.year}-${p.month}-${p.day} ${p.hour}:${p.minute}:${p.second}.${p.millisecond}")

match time.parse("0000-01-01T00:00:00Z") {
    Ok(_) {
        effect print("valid")
    }
    Err(message) {
        effect print(message)
    }
}

match time.parse("2026-07-26T09:15:00-00:00") {
    Ok(instant) {
        effect print(time.format(instant))
    }
    Err(message) {
        effect print(message)
    }
}

match time.parseDate("0000-01-01") {
    Ok(_) {
        effect print("valid")
    }
    Err(message) {
        effect print(message)
    }
}
```

Output:

```text
1969-12-31 23:59:59.999
timestamp out of range
2026-07-26T09:15:00Z
invalid date
```

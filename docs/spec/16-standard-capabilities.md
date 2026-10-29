# 16. Standard capabilities

The standard capabilities are the fixed set that every host offers in the same shape: `print`, `clock`, `random`, `stdin`, `args`, `env`, `fs` and `http`. A script written against one host reads the same against another. The rules for declaring, granting, scoping and calling capabilities are in [Chapter 9](09-effects-and-capabilities.md); this chapter defines what each one is.

Every capability function has an effectful type, so every call is written with `effect`. Types are given as `effect fn(...)`. A namespace capability is a record of such functions.

A host is not required to offer all of them. A host that offers one MUST offer it with exactly the types and behaviour below, and MUST refuse to grant a name in a shape different from this chapter's.

## 16.1 Shared types

These types are declared by the standard capabilities that mention them, by the rule of [section 9.7](09-effects-and-capabilities.md#97-the-capability-interface-of-a-run). `IoError` is declared by `stdin` and `fs`, and `HttpError` by `http`; in a run that grants one of those, a program can name the type in annotations and patterns without importing it. `HttpRequest` and `HttpResponse` give the shape of what `http` takes and returns and have no name of their own: a program that needs one declares a record with the same fields.

```
enum IoError {
    NotFound(String),
    Denied(String),
    AlreadyExists(String),
    Invalid(String),
    Failed(String),
}

enum HttpError {
    Denied(String),
    Invalid(String),
    Failed(String),
    Timeout(String),
    TooLarge(String),
}

record HttpRequest = { method: String, url: String, headers: Map<String, String>, body: String? }
record HttpResponse = { status: Int, headers: Map<String, String>, body: String }
```

The `String` payload of an error is a message for a person, supplied by the host. Its wording is not part of this specification, and a program should choose its behaviour by the variant and treat the message as text to show. The meaning of the variants:

| Variant | Meaning |
|---|---|
| `NotFound` | the path does not exist |
| `Denied` | the grant does not allow the operation ([section 9.5](09-effects-and-capabilities.md#95-scoped-grants)) |
| `AlreadyExists` | the target already exists in a way that prevents the operation |
| `Invalid` | the argument is not acceptable (a malformed path or URL, a method that does not exist, text that is not valid UTF-8) |
| `Failed` | the operation could not be completed for another reason |
| `Timeout` | the host's time limit for the request passed |
| `TooLarge` | the response is larger than the host allows |

## 16.2 print

```
print: effect fn(String)
```

`print(text)` writes the UTF-8 bytes of `text` followed by one LF byte to the host's standard output. A host that has no standard output decides where the text goes (an editor's console, a log). The bytes count against the output limit ([section 11.6](11-resource-limits.md#116-output)). `print` is the only way for a program to produce visible output.

Granted by default; not scoped.

## 16.3 clock

```
clock: {
    now: effect fn(): String,
    today: effect fn(): String,
    unixMillis: effect fn(): Int,
    monotonicMillis: effect fn(): Int,
}
```

- `clock.now()` is the current instant in UTC as RFC 3339 text with second precision, `YYYY-MM-DDTHH:MM:SSZ`, for example `2026-07-26T09:15:00Z`. The text is accepted by `time.parse` ([section 15.2](15-stdlib-data.md#152-time)).
- `clock.today()` is the current calendar date in UTC as `YYYY-MM-DD`. The same string is what filter expressions use for `today` ([Appendix A](appendix-a-filters.md)), so dates cross between the two without conversion.
- `clock.unixMillis()` is the current instant as an instant in the sense of [section 15.2](15-stdlib-data.md#152-time).
- `clock.monotonicMillis()` is a count of milliseconds from an arbitrary origin that never decreases during the life of the host process. Differences between two readings measure elapsed time.

The clock is the host's: a host may supply a fixed or simulated clock, so that programs are deterministic in tests. Granted by default; not scoped.

## 16.4 random

```
random: {
    int: effect fn(Int, Int): Int,
    float: effect fn(): Float,
}
```

- `random.int(min, upTo)` is an integer `n` with `min <= n < upTo`, each value equally likely. It faults with HS1011 if `min >= upTo`.
- `random.float()` is a `Float` `x` with `0.0 <= x < 1.0`, uniformly distributed.

The generator is the host's. The specification does not define the algorithm, does not promise cryptographic strength, and does not promise the same sequence on different hosts; a host may seed a generator explicitly so that a program is repeatable. Granted by default; not scoped.

## 16.5 stdin

```
stdin: {
    readLine: effect fn(): Result<String?, IoError>,
    readAll: effect fn(): Result<String, IoError>,
}
```

- `stdin.readLine()` reads the next line of the host's standard input. It returns `Ok` with the line without its terminator (LF or CR LF), or `Ok(none)` when the input is exhausted. A last line with no terminator is returned normally, and a CR that is not followed by LF is part of the line.
- `stdin.readAll()` reads everything that is left and returns it as one string (empty at the end of the input).

A line that is not valid UTF-8 is consumed and gives `Err(Invalid(_))`; `readAll` on input that is not valid UTF-8 gives `Err(Invalid(_))` and consumes it. Not granted by default; not scoped. The `Ok(none)` case is the value `none` in an optional `String`, matched as `Ok(none)`.

## 16.6 args

```
args: effect fn(): [String]
```

`args()` is the list of command-line arguments the host passes to the program, not including the name of the program itself. It is empty if there are none. Not granted by default; not scoped.

## 16.7 env

```
env: effect fn(String): String?
```

`env(name)` is the value of the environment variable `name`, or `none` if it is not set. Read-only. An empty name, and a name that contains `=` or U+0000, gives `none`. Names are compared as the platform's environment compares them, and the grant list is compared in the same way. If the grant lists variable names and `name` is not among them, the result is `none`, exactly as if the variable were unset, so a program cannot find out which variables exist. Not granted by default. Scope: an optional list of variable names; with no list, every variable is readable.

## 16.8 fs

```
fs: {
    read: effect fn(String): Result<String, IoError>,
    write: effect fn(String, String): Result<Int, IoError>,
    append: effect fn(String, String): Result<Int, IoError>,
    exists: effect fn(String): Result<Bool, IoError>,
    list: effect fn(String): Result<[String], IoError>,
    remove: effect fn(String): Result<Bool, IoError>,
    mkdir: effect fn(String): Result<Bool, IoError>,
}
```

Files are text: they are read and written as UTF-8 with no translation of line endings. Paths are strings that use `/` as the separator on every platform; a relative path is resolved against the host's base directory, which is the host's working directory unless it says otherwise.

| Function | Meaning | Needs |
|---|---|---|
| `fs.read(path)` | the contents of the file as a string. `Err(NotFound)` if absent, `Err(Invalid)` if the bytes are not valid UTF-8 | read |
| `fs.write(path, contents)` | creates the file or replaces its contents; `Ok` of the number of bytes written; the parent directory must exist (`Err(NotFound)` otherwise) | write |
| `fs.append(path, contents)` | adds `contents` at the end, creating the file if absent; `Ok` of the number of bytes appended | write |
| `fs.exists(path)` | whether a file or directory exists at `path` | read |
| `fs.list(path)` | the names (not paths) of the entries of a directory, in ascending code point order, without `.` and `..`; names that are not valid UTF-8 are left out; `Err(NotFound)` if there is no such directory | read |
| `fs.remove(path)` | removes a file or an empty directory; `Ok(true)` if something was removed, `Ok(false)` if there was nothing at `path`; `Err(Failed)` for a directory that is not empty | write |
| `fs.mkdir(path)` | creates the directory and any missing parents; `Ok(true)` if a directory was created, `Ok(false)` if the directory already existed; `Err(AlreadyExists)` if a file is there | write |

**Grants.** `fs` is granted in two independent kinds, `read` and `write` ([section 9.5](09-effects-and-capabilities.md#95-scoped-grants)), each with a list of paths that is required: `fs:read=./data`, `fs:write=./out`. A grant of `fs` with no kind, or of a kind with no path, grants nothing and is reported by the host as an error. Not granted by default.

**Malformed paths and wrong kinds.** A path is malformed, and the call returns `Err(Invalid(_))`, if it is empty, contains U+0000 or a backslash, begins with a drive designator (a letter and `:`), or has a segment other than `.` and `..` that ends in `.` or in a space. This applies on every platform and is checked first, before the scope check. Once the scope check has passed, an operation that meets the wrong kind of entry returns `Err(Invalid(_))`: `read` of a directory, `write` or `append` to a directory, `list` of a file, a path with a trailing `/` that names a file, and a path whose parent is a file. `append` whose parent directory is missing returns `Err(NotFound(_))`, as `write` does. `remove` of a directory that is not empty returns `Err(Failed(_))`. Any other failure is `Err(Failed(_))`. The order is therefore: `Invalid`, then `Denied`, then the operation. `list` includes hidden entries and symbolic links, by name, and `write` returns the number of bytes of the UTF-8 that it wrote.

**Scope check.** The host resolves the argument to a canonical absolute path by resolving `.` and `..` segments and every symbolic link, and by resolving the deepest existing ancestor when the path itself does not exist yet. The operation is allowed if and only if the canonical path is equal to a granted path (canonicalised the same way when the grant was made) or lies inside it, where "inside" means the granted path is a prefix at a path segment boundary, and the comparison is the one the file system itself makes: it is case-insensitive on a case-insensitive volume. A symbolic link that is dangling in any component leaves the scope, so it is `Denied`, and so is a path that leaves a granted directory by `..` or by a symbolic link. Otherwise the call returns `Err(Denied(_))` without any other effect, including for `exists`, so that a program cannot use `exists` to learn about paths it may not read.

**The check and the operation are one operation.** The host opens the granted directory once, as a handle, and performs every access relative to that handle without following a link out of it (for example with a resolve-beneath open, or by walking the components and refusing links), and never by opening again the path string it checked. A link that is swapped after the check therefore cannot lead outside the granted directory.

A file larger than the remaining memory allowance faults with HS1002 before its contents are read ([section 9.6](09-effects-and-capabilities.md#96-capability-values)).

```hollow
uses fs, print

constant written = effect fs.write("notes/list.txt", "buy milk\n")
match written {
    Ok(bytes) {
        effect print("wrote ${bytes} bytes")
    }
    Err(Denied(_)) {
        effect print("not allowed")
    }
    Err(_) {
        effect print("failed")
    }
}
```

Run with the grant `fs:write=notes` and an existing `notes` directory, this prints `wrote 9 bytes`.

```hollow
uses fs

fn describe(e: IoError): String {
    match e {
        NotFound(path) {
            return "missing: ${path}"
        }
        Denied(path) {
            return "denied: ${path}"
        }
        else {
            return "io failure"
        }
    }
}

fn lineCount(path: String): Result<Int, String> {
    constant text = (effect fs.read(path)).mapError(describe)?
    return success(text.lines().length())
}

return lineCount("data/input.txt")
```

With `fs:read=data` this returns `Ok` of the number of lines of `data/input.txt`, and `Err("missing: ...")` if the file is absent.

## 16.9 http

```
http: {
    get: effect fn(String): Result<HttpResponse, HttpError>,
    request: effect fn(HttpRequest): Result<HttpResponse, HttpError>,
}
```

`http.get(url)` is `http.request` with the method `GET`, no headers and no body.

**Request.** `url` must be an absolute `http` or `https` URL with a host and no user information; otherwise `Err(Invalid)`. `method` must be one of `GET`, `HEAD`, `POST`, `PUT`, `PATCH`, `DELETE` and `OPTIONS`, in upper case, otherwise `Err(Invalid)`. `headers` maps header names to values; names are matched case-insensitively. `body` is sent as UTF-8; `none` sends no body. Requests to `https` URLs verify the server certificate, and there is no way to switch that off.

**Response.** A response with any status is returned as `Ok`: a `404` or a `500` is not an error to the language, and the program decides. `status` is the status code. `headers` has one entry per header name, with the name in lower case and, if the response repeated a name, the values joined with `, ` in order. `body` is the body decoded as UTF-8; a body that is not valid UTF-8 is `Err(Failed)`. A charset parameter of the response is ignored, and a leading byte order mark is kept. The host sends no `Accept-Encoding` of its own; if a request sets one, the host decompresses the body before it decodes it. The `body` of the response to `HEAD`, and of a `204`, a `304` and every `1xx` status, is `""`. Two request header names that are the same after lower-casing, and a name or value that contains CR, LF or U+0000, make the request `Err(Invalid)`. A `body` given with `GET` or `HEAD` is sent.

**Redirects.** The host follows at most five redirects (statuses 301, 302, 303, 307 and 308). For 301, 302 and 303 a request that was not `GET` or `HEAD` becomes a `GET` without a body; 307 and 308 repeat the request unchanged. Every redirect target is checked against the grant as if it had been requested directly, and a target outside the grant is `Err(Denied)`. The target is resolved against the request URL by RFC 3986; a redirect from `https` to `http` is followed only if the grant allows the `http` target, and a redirect with no `Location`, or one that cannot be parsed, returns the response as `Ok`. More than five redirects is `Err(Failed)`.

**Limits.** The host applies a total time limit to each request (30 seconds by default, `Err(Timeout)` when it passes) and a maximum body size (8 MiB by default, `Err(TooLarge)` beyond it). Hosts may change these defaults for a run, and they are reported through the same variants.

**Grants.** `http` is granted with a required list of host names: `http=api.example.com,*.cdn.example.com`. A scope value is:

- a host name, matching that host exactly, compared as lower-case ASCII (an internationalised host name is compared in its `xn--` form);
- `*.` followed by a host name, matching any host name that has that name as a proper suffix after a dot (`*.example.com` matches `a.example.com` and `a.b.example.com`, not `example.com`);
- an IP address written literally (an IPv6 address in brackets, `[::1]`), matching only that address; a host name never matches an address it resolves to;
- optionally followed by `:` and a port number, restricting the port. Without a port, only the default ports 80 (for `http`) and 443 (for `https`) are allowed, and an explicit default port in a request URL (`https://h:443`) is the same as none.

A trailing dot on a host name is removed before matching.

A request whose host does not match any scope value returns `Err(Denied(_))` before any connection is made. A grant with no host list grants nothing and is reported as an error by the host. Not granted by default.

**Addresses.** The host parses the URL once. A host written in a numeric form that is not the canonical dotted-decimal IPv4 or bracketed IPv6 form (`2130706433`, `0x7f.1`, `127.1`, a zone identifier) is `Err(Invalid)`, and an IPv4-mapped IPv6 address is reduced to its IPv4 address before matching. For a host name the host resolves the name once and connects only to an address of that answer, with the granted name as the `Host` header and as the TLS name; it never resolves again between the check and the connection. By default it refuses an address that is loopback, private, link-local, multicast or a cloud metadata address, returning `Err(Denied(_))`, unless the grant is an IP literal that matches that address. The same rules apply to every redirect target.

```hollow
uses http, print

constant response = effect http.get("https://api.example.com/status")
match response {
    Ok(r) {
        effect print("status ${r.status}")
    }
    Err(Denied(_)) {
        effect print("host not allowed")
    }
    Err(_) {
        effect print("request failed")
    }
}
```

Run with `http=api.example.com`.

## 16.10 Summary

| Name | Shape | Granted by default | Scope |
|---|---|---|---|
| `print` | function | yes | none |
| `clock` | namespace | yes | none |
| `random` | namespace | yes | none |
| `stdin` | namespace | no | none |
| `args` | function | no | none |
| `env` | function | no | optional variable names |
| `fs` | namespace | no | required paths, kinds `read` and `write` |
| `http` | namespace | no | required host names |

The three that are granted by default cannot read or change anything the person running the script needs to protect: `print` writes only to the process's own output, `clock` reveals only the time, and `random` only numbers the host itself produced. A script that is run with no grants can therefore print, read the clock and get random numbers, and cannot read or write a file, use the network, or read its environment, whatever its `uses` lines ask for.

A program written to run in an environment where `print` is not offered (a browser sandbox with no console) is not affected by the language: the host simply does not grant it, and `uses print` is then HS0404.

## 16.11 Host-specific capabilities

Everything not listed here is defined by the host: a task store, a notifier, drawing and input for a game. The host declares each as a capability interface ([section 9.6](09-effects-and-capabilities.md#96-capability-values)), and to the language it is identical to the standard ones. A host-specific capability that accepts a filter takes a parameter of type `Filter` and receives the validated filter ([Appendix A](appendix-a-filters.md)).

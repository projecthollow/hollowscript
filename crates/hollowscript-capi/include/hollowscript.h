/* The C ABI of hollowscript-capi.
 *
 * This header is hand-written and kept in lock-step with
 * crates/hollowscript-capi/src/lib.rs, which is the normative definition of
 * the ABI; this file exists only so that a C or C++ host does not have to
 * transcribe it. Every declaration here has a matching Rust item whose own
 * doc comment is the real specification of what it does, its ownership
 * rules and its safety contract; read that for anything this header's
 * shorter comments leave out.
 *
 * Link against the `hollowscript_capi` static or dynamic library this crate
 * builds (`cargo build -p hollowscript-capi`), which has no third-party
 * dependency of its own.
 */
#ifndef HOLLOWSCRIPT_CAPI_H
#define HOLLOWSCRIPT_CAPI_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Status codes every function below returns, except the ones that cannot
 * fail (hs_version) or that report success by construction (hs_dispose,
 * hs_string_free, the hs_cancel_token_* functions). */
typedef enum HsStatus {
    HS_OK = 0,
    HS_NULL_ARGUMENT = 1,
    HS_INVALID_UTF8 = 2,
    HS_CHECK_FAILED = 3,
    HS_REFUSED = 4,
    HS_FAULTED = 5,
    HS_PANICKED = 6,
    HS_UNKNOWN_HANDLE = 7,
    /* An HsCapabilityFn's params_json or result_json was non-null and
     * non-empty but was not valid JSON, or not one of the type descriptions
     * documented above HsCapabilityFn (a malformed capability declaration,
     * distinct from HS_CHECK_FAILED, which is a malformed program); or one
     * of params, result_shape, result or result_err held a value that is
     * not one of HsValueType's or HsResultShape's own discriminants. The
     * reason is written to the caller's output. */
    HS_INVALID_CAPABILITY = 8,
    /* An HsLimits passed to hs_load_ex held a value outside 1 to 2^63 - 1
     * (section 11.1: a limit cannot be switched off, and 0 is not a
     * value). The reason is written to the caller's output. */
    HS_INVALID_LIMITS = 9,
} HsStatus;

/* An opaque instance handle (chapter 12 of the specification): a host holds
 * this between calls and releases it with hs_dispose. It is an identifier
 * the library looks up, never a pointer it follows, so a stale or made-up
 * handle is HS_UNKNOWN_HANDLE rather than undefined behaviour. The counter
 * behind it is pointer-width: on every target this library ships for today
 * that is 64 bits, wide enough that it cannot wrap in a process's real
 * lifetime, so a stale handle can never coincide with a later instance's.
 * On a hypothetical 32-bit target it is only 32 bits, and a host loading
 * more than roughly four billion times over one process's life should
 * restart the process before then, since a very old stale handle could
 * otherwise coincide with one this library hands out much later. */
typedef struct HsInstance HsInstance;

/* An opaque cancellation token (section 11.7), created with
 * hs_cancel_token_new. Cancelling it is the one operation this library
 * allows to run concurrently, from another thread, with the hs_load_ex or
 * hs_call_ex call it was passed to. */
typedef struct HsCancelToken HsCancelToken;

/* The library's own version, as a static, null-terminated string with no
 * allocation. Compare this against the version a host was built against to
 * detect a mismatched library. */
const char *hs_version(void);

/* Every diagnostic this header's *_json functions answer carries what the
 * facade's own Diagnostic has:
 *
 *   {"severity":"error","code":"HS1099","message":"...",
 *    "file":"main.hws","line":1,"column":5,
 *    "spanStartUtf16":12,"spanEndUtf16":19,
 *    "related":[{"file":"main.hws","line":2,"column":1,
 *                "spanStartUtf16":20,"spanEndUtf16":25,"message":"..."}]}
 *
 * "line" and "column" are the specification's own units: 1-based, "column"
 * in code points. "spanStartUtf16"/"spanEndUtf16" are the half-open span in
 * UTF-16 code units, not bytes and not code points, named explicitly
 * because that is what a web text editor such as Monaco counts a position
 * in; convert to another unit only if the host's own editor needs one.
 * "file"/"line"/"column"/"spanStartUtf16"/"spanEndUtf16" are null for a
 * diagnostic (or a related location) with no position at all: the host
 * errors HS1201 to HS1205, which answer a host operation, not a position
 * in a program. "related" is always present, empty when there are none. */

/* Checks `source` (the entry module) and writes a JSON array of diagnostics,
 * in the shape above, to `*out_json`. `entry_name` may be NULL, meaning
 * "main.hws". On HS_OK, `*out_json` is a string the caller must free with
 * hs_string_free. */
int hs_check_json(const char *source, const char *entry_name, char **out_json);

/* Formats `source` to its canonical layout and writes a JSON document to
 * `*out_json`: {"output":[...lines...]|null,"diagnostics":[...]}, "output"
 * null only when the source has a syntax error the formatter cannot
 * recover from, and "diagnostics" each in the shape above. On HS_OK,
 * `*out_json` is a string the caller must free with hs_string_free. */
int hs_format_json(const char *source, const char *entry_name, char **out_json);

/* Frees a string this crate returned (from any *_json or *_text output
 * parameter, or from an hs_call/hs_read/hs_call_ex result). Freeing NULL is
 * a no-op. Freeing a pointer this crate did not return, or freeing one
 * twice, is undefined behaviour, exactly as for any other owned
 * allocation: never pass a pointer here that a capability callback wrote to
 * out_json or out_error, since the host keeps ownership of those.
 *
 * The string's content, once returned, is the host's to read but not to
 * modify: writing into it, including writing an early '\0' to use it as a
 * shorter or truncated string in place, is undefined behaviour once this is
 * later called on the same pointer. This function finds the string's own
 * length again by scanning for its NUL terminator, then frees exactly that
 * many bytes; an earlier '\0' a host wrote makes that scan stop short of
 * the allocation's real, original size, so the deallocator is asked to
 * free fewer bytes than it allocated, which corrupts the heap rather than
 * merely leaking. A host that wants a shorter string makes its own copy of
 * the prefix it needs and frees the original, unmodified, exactly as it
 * was returned. */
void hs_string_free(char *ptr);

/* Loads `source` as the entry module `entry_name` and writes an opaque
 * instance handle to `*out_instance` (section 12.3). A program with a
 * checker error creates no instance: the return is HS_CHECK_FAILED,
 * `*out_instance` is left NULL and `*out_json` holds the diagnostics.
 * Nothing is granted: this overload has no way to supply a capability, so a
 * program that `uses` one is HS0404; use hs_load_ex to grant capabilities.
 *
 * A program whose *initialisation* faults (a top-level `1 / 0`, for one)
 * still produces a live instance, in the Faulted state (section 12.2): the
 * return is HS_FAULTED, `*out_instance` is the caller's handle exactly as
 * on success, and `*out_json` holds the fault as a one-element diagnostics
 * array, the same shape hs_check_json itself uses (section 12.1's own
 * promise that a load's fault is answered "as often as asked"). The
 * instance still needs hs_dispose; a call on it is refused (HS_REFUSED), as
 * on any other Faulted instance.
 *
 * A program with no error, HS_OK or HS_FAULTED either way, may still have
 * its own warnings (HS0801 "unused constant or variable", for one):
 * `*out_json` then holds those too, in the same diagnostics array, after
 * the fault if there is one. `*out_json` is the empty array "[]" only when
 * there is neither a fault nor a warning.
 *
 * On success (HS_OK), `*out_instance` is owned by the caller and released
 * with hs_dispose; on HS_CHECK_FAILED or HS_FAULTED, `*out_json` is owned
 * by the caller and released with hs_string_free (and, on HS_FAULTED, so
 * is `*out_instance`, separately, with hs_dispose). */
int hs_load(
    const char *source,
    const char *entry_name,
    HsInstance **out_instance,
    char **out_json);

/* Every value chapter 12 transfers across this ABI, in either direction,
 * crosses as JSON, using the same encoding hollowscript-wasm's own JS
 * boundary uses:
 *
 *   Int, Float, Bool  -> a JSON number or true/false (Float always keeps a
 *                        decimal point, 3.0 not 3, so it is never read back
 *                        as an Int)
 *   String            -> a JSON string
 *   none               -> null
 *   [T]                -> {"list": [...]}
 *   Set<T>             -> {"set": [...]}
 *   Map<K, V>          -> {"map": [[k, v], ...]}, an array of two-element
 *                        [key, value] arrays, not a JSON object, because a
 *                        key is not always object-safe (it may be Int or
 *                        Bool)
 *   a record           -> {"record": {"field": value, ...}}
 *   an enum value       -> {"enum": {"name": "E", "variant": "V",
 *                        "payload": [...]}}
 *   Result<T, E>       -> sugar for the Result enum specifically: {"Ok":
 *                        value} or {"Err": value}
 *   Filter             -> {"filter": "<source text>"}, one direction only:
 *                        a program builds a Filter only from its own
 *                        backtick literal, never from a value a host hands
 *                        back
 *
 * A record or enum value crosses only when its shape or variant is one the
 * module already has, either because a capability's own declared type
 * mentions it or because the program's own source does anywhere, including
 * only in an exported function's own signature.
 *
 * A capability's own declared parameter and result types (HsCapabilityFn,
 * below) use the same grammar, one level up: params/result/result_err
 * remain the four HsValueType scalars plus Filter, but params_json and
 * result_json (see their own comments) declare a parameter or result of
 * any transferable type at all, the same tags the value encoding above
 * uses for a value of that type.
 *
 * A bare JSON number is decoded against the declared type, not its own
 * spelling. JSON cannot tell 3 from 3.0 (a number with no '.' and no
 * exponent is read as an integer, whichever the sender meant), and a
 * JavaScript host cannot write one that can: JSON.stringify(3.0) is
 * already "3" by the time this ABI ever sees it. So a JSON number crossing
 * inbound (an hs_call argument, or a capability's own answer) decodes to
 * the Float its position's declared type calls for even when it looks
 * integral, walking into a list element, a map's own value, a record
 * field, an enum's or Result's own payload and an optional's own inner
 * type the same way; a number at a declared Int position, one outside the
 * Int range, or one that is not integral where Int is declared, is still
 * refused, by the same acceptance rule as before (section 9.6). An integer
 * that looks integral at a Float position but is not exactly representable
 * as one (outside -2^53 to 2^53, section 3.4's own limit on a Float's
 * integer precision) is refused the same way, not rounded to the nearest
 * Float: 9007199254740993 is refused, never silently read as
 * 9007199254740992.0. This is
 * the encoding's own definition of which language value a JSON number
 * denotes, given the declared type; it is a different question from
 * section 9.6's "nothing is converted inside a collection, a record or an
 * enum payload", which is about a language value the boundary has already
 * decided on, not about deciding what one JSON number means in the first
 * place. */

/* Calls the exported function `name` of `instance` with `arguments_json` (a
 * JSON array of values in the encoding above, e.g.
 * "[1, \"a\", true, null, {\"list\": [1, 2]}]") and writes the rendered
 * result to `*out_text` (section 12.4): "Int(3)", "String(\"a\")", or the
 * empty string for a function that returns nothing. A call the instance
 * refuses (an unknown export, the wrong number of arguments, an instance
 * that is not Ready) answers HS_REFUSED with the reason in `*out_text`; one
 * that runs and faults answers HS_FAULTED with the fault's message.
 * `*out_text` is always owned by the caller and released with
 * hs_string_free. */
int hs_call(
    HsInstance *instance,
    const char *name,
    const char *arguments_json,
    char **out_text);

/* Reads the exported constant `name` of `instance` and writes its rendering
 * to `*out_text`, exactly as hs_call's result. */
int hs_read(HsInstance *instance, const char *name, char **out_text);

/* Writes the load result (section 12.1 and 12.4: the value a top-level
 * `return` in the entry module produced, rendered the same way hs_call's
 * own result is) to `*out_text`. A Ready instance (no top-level `return`
 * was reached yet, or there was none) answers the empty string, every time;
 * a Finished instance answers the rendered value the first time this is
 * called and HS_REFUSED, with an explanatory message in `*out_text`, on
 * every call after that (the load result is read once, matching
 * hollowscript-wasm's own loadResult). Called on an instance that is
 * neither Ready nor Finished (Running, or already disposed), this also
 * answers HS_REFUSED. `*out_text` is always owned by the caller and
 * released with hs_string_free. */
int hs_load_result(HsInstance *instance, char **out_text);

/* Ends `instance` and releases everything it holds (section 12.5). A handle
 * that is not live (NULL, never given out, or already released) is
 * ignored, so a host may call this unconditionally and more than once. */
void hs_dispose(HsInstance *instance);

/* One of the scalar types a host capability's parameters and result may be
 * (section 9.6's transferable types, restricted the same way hs_call's
 * arguments already are: no list, map, record or enum, because a C caller
 * has no way to name one of those through this enum, only through the
 * module's own compiled type table; see the encoding comment above
 * hs_call).
 *
 * HS_TYPE_FILTER is the one exception, and is a parameter-only type
 * (Appendix A.1): a program's own filter literal is the only way a Filter
 * value is ever built, so it is never a capability's result or result_err,
 * only ever one of its params. A capability that declares it there anyway
 * is not rejected by this library; the checker itself reports it (chapter
 * 9). */
typedef enum HsValueType {
    HS_TYPE_INT = 0,
    HS_TYPE_FLOAT = 1,
    HS_TYPE_BOOL = 2,
    HS_TYPE_STRING = 3,
    HS_TYPE_FILTER = 4,
} HsValueType;

/* What an HsCapabilityFn answers (section 8.2 and 9.6). */
typedef enum HsResultShape {
    /* The function returns nothing; result and result_err are unread. */
    HS_RESULT_NONE = 0,
    /* A single scalar of `result`. */
    HS_RESULT_VALUE = 1,
    /* Result<result, result_err> (section 8.2): the callback answers
     * {"Ok": <scalar of result>} or {"Err": <scalar of result_err>} instead
     * of a bare scalar; see HsCapabilityFn::call below. */
    HS_RESULT_RESULT = 2,
} HsResultShape;

/* A capability function a C host supplies to hs_load_ex (chapter 9).
 *
 * A plain function capability (`print`) has one HsCapabilityFn with
 * `function` NULL. A namespace capability (`clock`, with `now` and
 * `today`) has one entry per function, all sharing `name` and each naming
 * itself with `function`; hs_load_ex groups entries by `name`, and a group
 * of more than one member requires every member to name a `function`.
 *
 * Ownership: `name`, `function` and `params` are read once, while
 * hs_load_ex builds the capability interface, and are not retained past
 * that call returning. `call` and `user_data` are retained for the life of
 * the instance hs_load_ex creates and must stay valid until it is
 * disposed. */
typedef int32_t (*HsCapabilityCall)(
    void *user_data,
    const char *args_json,
    char **out_json,
    char **out_error);

/* A type description, as HsCapabilityFn's own params_json and result_json
 * write it: a bare JSON string for one of the five scalars ("Int", "Float",
 * "Bool", "String", "Filter"; "Filter" valid only within params_json, per
 * Appendix A.1's own parameter-only restriction, which the checker enforces,
 * not this parser), or a JSON object with exactly one field naming a
 * container kind and describing its own element type(s) the same way,
 * recursively:
 *
 *   {"list": T}, {"set": T}, {"optional": T}
 *   {"map": {"key": T, "value": T}}
 *   {"record": {"field": T, ...}}, at least one field
 *   {"enum": {"name": "E", "variants": {"V": [T, ...], ...}}}, at least one
 *       variant; a payload-less variant still needs its array, empty
 *   {"ok": T, "err": T}, Result<T, E> (section 8.2), kept as a bare
 *       two-field object rather than a single-field tag
 *
 * This is the same tagged shape the value encoding above hs_call uses, one
 * level up: a capability whose declared result type is
 * {"record": {"x": "Int", "y": "Int"}} answers a value tagged the same way,
 * {"record": {"x": 3, "y": 4}}. It is also exactly the grammar
 * hollowscript-wasm's own tooling build (crate::tooling::declared_type)
 * uses for a page's capability declarations, so a capability interface
 * written once, as JSON, reads the same at either boundary. */

typedef struct HsCapabilityFn {
    /* The capability's `uses` name: a non-null, null-terminated, valid
     * UTF-8 C string. */
    const char *name;
    /* The function's own name, within a namespace capability's several
     * ("now" of `clock`); NULL for a capability that is a single function,
     * not a namespace. */
    const char *function;
    /* The parameter types, by position, each one of the HsValueType
     * discriminants above (HS_TYPE_INT to HS_TYPE_FILTER). Declared
     * int32_t, not HsValueType itself, so that the library reads it with a
     * checked conversion rather than reinterpreting an arbitrary integer as
     * a Rust enum, which is undefined behaviour when the value names none
     * of the five; an out-of-range entry is refused as
     * HS_INVALID_CAPABILITY, not read. May be NULL only when params_len is
     * zero. Ignored (may be left however the host likes) when params_json
     * is non-NULL and non-empty; see params_json below. */
    const int32_t *params;
    size_t params_len;
    /* What the function answers: one of the HsResultShape discriminants
     * above (HS_RESULT_NONE to HS_RESULT_RESULT). Declared int32_t for the
     * same reason params is. An out-of-range value is refused as
     * HS_INVALID_CAPABILITY. */
    int32_t result_shape;
    /* The result type when result_shape is HS_RESULT_VALUE, or Result's
     * success type when it is HS_RESULT_RESULT, one of the HsValueType
     * discriminants; declared int32_t for the same reason. Unread when
     * result_shape is HS_RESULT_NONE, or when result_json is non-NULL and
     * non-empty; see result_json below. */
    int32_t result;
    /* Result's failure type, the same encoding as result; read only when
     * result_shape is HS_RESULT_RESULT and result_json is NULL or empty. */
    int32_t result_err;
    /* The parameter types, by position, as a JSON array of type
     * descriptions (see the comment above this struct), for a parameter
     * `params`'s five scalars cannot express (a record, a list, and so on).
     * NULL, or the empty string, means "use params/params_len instead";
     * non-NULL and non-empty takes over entirely, and params/params_len are
     * then not read at all. Read once, while hs_load_ex builds the
     * capability interface, and not retained past that call returning,
     * exactly as params itself is not. */
    const char *params_json;
    /* The result type, as one type description (see the comment above this
     * struct), for a result `result` cannot express, including Result<T, E>
     * with a non-scalar ok/err, written {"ok": T, "err": T}. NULL, or the
     * empty string, means "use result_shape/result/result_err instead";
     * non-NULL and non-empty takes over entirely for the return type (but
     * result_shape must still not be HS_RESULT_NONE, or this function has
     * no return type and result_json is ignored). Read once, the same as
     * params_json. */
    const char *result_json;
    /* Called when the program calls this capability. `args_json` is a JSON
     * array of the arguments, in the encoding documented above hs_call
     * (params_len entries, each a value of the corresponding params[i], so
     * a HS_TYPE_FILTER parameter arrives as {"filter": "<source text>"}).
     *
     * On success the callback returns 0 and, unless result_shape is
     * HS_RESULT_NONE, writes its answer to *out_json: a single JSON value
     * (in that same encoding) for HS_RESULT_VALUE, or, for
     * HS_RESULT_RESULT, a JSON object with exactly one field, either
     * {"Ok": <value>} or {"Err": <value>}, exactly as a capability declared
     * Result<T, E> answers success or an expected failure as a plain value
     * the program handles (section 9.5's own example: fs.read answers
     * Err(IoError.NotFound(message)) rather than failing the call). On
     * failure the callback returns a non-zero value and may write a message
     * to *out_error; this is a different thing from a Result's Err, and
     * reaches the program as the fault HS1012 rather than a value it can
     * pattern-match. Neither output pointer is read unless the
     * corresponding condition holds.
     *
     * Ownership of *out_json and *out_error: the callback keeps ownership
     * of whatever it points them at. This library reads the string during
     * the call and copies it, and never frees it; the pointer only needs
     * to stay valid until the callback itself returns.
     *
     * `call` must return normally, every time, even on failure (a non-zero
     * result is a normal return). It must never unwind past its own return:
     * a C++ exception that escapes it, or a longjmp past it, does not run
     * this library's own cleanup on the way out. The instance is left
     * marked busy for good: every later call on it is refused, and it can
     * never actually be freed either, only leaked for the rest of the
     * process. This is inherent to crossing this ABI in either direction,
     * not a defect the library can catch the way it catches its own Rust
     * panics. */
    HsCapabilityCall call;
    /* Passed back to `call` unchanged; this library never reads through
     * it. */
    void *user_data;
} HsCapabilityFn;

/* The four resource limits of one run (chapter 11), settable through
 * hs_load_ex's own `limits` parameter: each governs the whole load,
 * including the initialisation run, and every call made afterwards on the
 * instance it produces. Read once, while hs_load_ex builds the instance,
 * and not retained past that call returning.
 *
 * Passing NULL for hs_load_ex's own `limits` parameter uses the
 * specification's own defaults (100,000,000 steps, 64 MiB memory, 16 MiB
 * output, call depth 1,000), exactly as before this type existed. Each
 * field must be from 1 to 2^63 - 1 (section 11.1: a limit cannot be
 * switched off, and 0 is not a value); one outside that range is refused
 * as HS_INVALID_LIMITS before the load is attempted, with the reason in
 * hs_load_ex's own *out_json. */
typedef struct HsLimits {
    uint64_t max_steps;
    uint64_t max_memory_bytes;
    uint64_t max_output_bytes;
    uint64_t max_call_depth;
} HsLimits;

/* As hs_load, but a C host may also declare and bind capabilities (chapter
 * 9), supply a cancellation token (created with hs_cancel_token_new) that
 * governs the whole load, including the initialisation run, and set the
 * run's own resource limits (see HsLimits above). Pass capabilities_len = 0
 * and capabilities = NULL for a program that uses no capability,
 * cancel_token = NULL for a load that never needs cancelling, and
 * limits = NULL for the specification's own defaults. */
int hs_load_ex(
    const char *source,
    const char *entry_name,
    const HsCapabilityFn *capabilities,
    size_t capabilities_len,
    const HsCancelToken *cancel_token,
    const HsLimits *limits,
    HsInstance **out_instance,
    char **out_json);

/* As hs_call, but a C host may also supply a cancellation token: set it
 * from another thread while the call is in flight and the run stops at its
 * next safepoint (section 11.7) with the fault HS1006, reported here as
 * HS_FAULTED. Pass cancel_token = NULL for a call that never needs
 * cancelling. */
int hs_call_ex(
    HsInstance *instance,
    const char *name,
    const char *arguments_json,
    const HsCancelToken *cancel_token,
    char **out_text);

/* Creates a cancellation token, not yet set. The caller owns it and must
 * release it with hs_cancel_token_free once no hs_load_ex or hs_call_ex
 * call can still be reading it. */
HsCancelToken *hs_cancel_token_new(void);

/* Sets `token`, so that the operation it was passed to stops at its next
 * safepoint with the fault HS1006. This is the one function in this header
 * that is safe to call from a thread other than the one running
 * hs_load_ex/hs_call_ex, which is the whole point of a token: cancel a
 * long-running or runaway call from another thread while it is in flight. */
void hs_cancel_token_cancel(const HsCancelToken *token);

/* Releases a cancellation token. Freeing NULL is a no-op. Must not be
 * called while hs_load_ex or hs_call_ex might still read the token. */
void hs_cancel_token_free(HsCancelToken *token);

#ifdef __cplusplus
}
#endif

#endif /* HOLLOWSCRIPT_CAPI_H */

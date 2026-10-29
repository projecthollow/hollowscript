/* A small C host exercising hollowscript-capi's C ABI end to end: loading a
 * program that grants a host-defined capability, calling its exported
 * functions, reading a constant, and cancelling a running call from another
 * thread. Run by crates/hollowscript-capi/tests/c_integration.rs, which
 * compiles this file with `cc` and links it against the library this crate
 * builds; that test is skipped, not failed, when no C compiler is
 * available.
 *
 * Exit code 0 means every check below passed; anything else is printed to
 * stderr before exiting 1, which the Rust test reports as a failure.
 */
#include "hollowscript.h"

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int failed = 0;

#define CHECK(cond, message)                                                 \
    do {                                                                     \
        if (!(cond)) {                                                       \
            fprintf(stderr, "FAILED: %s (%s:%d)\n", (message), __FILE__,     \
                    __LINE__);                                               \
            failed = 1;                                                      \
        }                                                                    \
    } while (0)

/* Captures the text a "print" capability call was given, for the test to
 * inspect afterwards. */
static char captured_print[256];

static int32_t capture_print(void *user_data, const char *args_json,
                              char **out_json, char **out_error) {
    (void)out_json;
    (void)out_error;
    char *buffer = (char *)user_data;
    strncat(buffer, args_json, 255 - strlen(buffer));
    return 0;
}

/* A capability that squares its one Int argument, answering through
 * *out_json exactly like a program's own function would return a value. */
static int32_t square(void *user_data, const char *args_json, char **out_json,
                       char **out_error) {
    (void)user_data;
    (void)out_error;
    long long n = atoll(args_json + 1); /* skip the leading '[' */
    static char buffer[32];
    snprintf(buffer, sizeof(buffer), "%lld", n * n);
    *out_json = buffer;
    return 0;
}

/* A namespace capability's two functions ("clock.now", "clock.today"),
 * proving several functions can share one `uses` name. */
static int32_t clock_now(void *user_data, const char *args_json,
                          char **out_json, char **out_error) {
    (void)user_data;
    (void)args_json;
    (void)out_error;
    *out_json = "\"2026-09-23T12:00:00Z\"";
    return 0;
}

static int32_t clock_today(void *user_data, const char *args_json,
                            char **out_json, char **out_error) {
    (void)user_data;
    (void)args_json;
    (void)out_error;
    *out_json = "\"2026-09-23\"";
    return 0;
}

/* A capability declared Result<String, String> (section 8.2): "found" and
 * "missing" each take a key and answer {"Ok": ...} or {"Err": ...}, proving
 * a capability's own successful call can still answer the program's Result
 * type either way, distinct from the callback itself failing (HS1012). */
static int32_t cache_get(void *user_data, const char *args_json,
                          char **out_json, char **out_error) {
    (void)user_data;
    (void)out_error;
    if (strstr(args_json, "found") != NULL) {
        *out_json = "{\"Ok\": \"cached value\"}";
    } else {
        *out_json = "{\"Err\": \"no such key\"}";
    }
    return 0;
}

/* Captures the raw args_json a "tasks" capability call was given, for the
 * test to inspect afterwards (a Filter parameter, HS_TYPE_FILTER). */
static char captured_tasks[256];

static int32_t capture_tasks(void *user_data, const char *args_json,
                              char **out_json, char **out_error) {
    (void)out_json;
    (void)out_error;
    char *buffer = (char *)user_data;
    strncat(buffer, args_json, 255 - strlen(buffer));
    return 0;
}

/* Every transferable type of chapter 12 crosses this ABI as JSON (list,
 * map, set, record, enum, Filter), not only the four scalars: hs_call's
 * arguments and result, and a capability's own arguments and answer, all
 * use the one encoding documented above hs_call in hollowscript.h. This
 * also checks hs_load_result on a Ready instance (no top-level `return`):
 * it answers the empty string, since the load result is only ever a value
 * for a Finished instance (see test_load_result below). */
static void test_transferable_types(void) {
    captured_tasks[0] = '\0';
    int32_t tasks_params[1] = {HS_TYPE_FILTER};
    HsCapabilityFn capabilities[1];
    capabilities[0].name = "tasks";
    capabilities[0].function = NULL;
    capabilities[0].params = tasks_params;
    capabilities[0].params_len = 1;
    capabilities[0].result_shape = HS_RESULT_NONE;
    capabilities[0].result = HS_TYPE_INT;
    capabilities[0].result_err = HS_TYPE_INT;
    capabilities[0].call = capture_tasks;
    capabilities[0].user_data = captured_tasks;
    capabilities[0].params_json = NULL;
    capabilities[0].result_json = NULL;

    const char *source =
        "uses tasks\n"
        "record Point = { x: Int, y: Int }\n"
        "export fn sumList(xs: [Int]): Int {\n"
        "    variable total = 0\n"
        "    for x in xs {\n"
        "        total = total + x\n"
        "    }\n"
        "    return total\n"
        "}\n"
        "export fn area(p: Point): Int {\n"
        "    return p.x * p.y\n"
        "}\n"
        "export fn run() {\n"
        "    effect tasks(`priority:high`)\n"
        "}\n";

    HsInstance *instance = NULL;
    char *json = NULL;
    int status = hs_load_ex(source, "main.hws", capabilities, 1, NULL, NULL,
                             &instance, &json);
    CHECK(status == HS_OK, "hs_load_ex loads a program using the full JSON encoding");
    if (status != HS_OK && json != NULL) {
        fprintf(stderr, "diagnostics: %s\n", json);
    }
    CHECK(instance != NULL, "hs_load_ex produces an instance");
    hs_string_free(json);

    char *out = NULL;
    status = hs_load_result(instance, &out);
    CHECK(status == HS_OK && out != NULL && strcmp(out, "") == 0,
          "hs_load_result answers the empty string for a Ready instance");
    hs_string_free(out);

    out = NULL;
    status = hs_call_ex(instance, "sumList", "[{\"list\": [1, 2, 3]}]", NULL, &out);
    CHECK(status == HS_OK && out != NULL && strcmp(out, "Int(6)") == 0,
          "a list argument crosses as {\"list\": [...]}");
    hs_string_free(out);

    out = NULL;
    status = hs_call_ex(instance, "area", "[{\"record\": {\"x\": 3, \"y\": 4}}]",
                         NULL, &out);
    CHECK(status == HS_OK && out != NULL && strcmp(out, "Int(12)") == 0,
          "a record argument crosses as {\"record\": {...}}, "
          "even though Point is only ever named in area's own signature");
    hs_string_free(out);

    out = NULL;
    status = hs_call_ex(instance, "run", "[]", NULL, &out);
    CHECK(status == HS_OK, "run() succeeds");
    hs_string_free(out);

    CHECK(strcmp(captured_tasks, "[{\"filter\":\"priority:high\"}]") == 0,
          "a Filter capability argument crosses as its source text, tagged "
          "{\"filter\": \"...\"}");

    hs_dispose(instance);
}

/* hs_load_result on a Finished instance (section 12.1 and 12.4): a program
 * whose entry module executes a top-level `return` never reaches Ready, so
 * none of its exported functions can be called, but the value it returned
 * is available through hs_load_result exactly once. */
static void test_load_result(void) {
    const char *source = "export constant unused = 1\nreturn 99\n";
    HsInstance *instance = NULL;
    char *json = NULL;
    int status = hs_load(source, "main.hws", &instance, &json);
    CHECK(status == HS_OK, "hs_load loads a program with a top-level return");
    CHECK(instance != NULL, "hs_load produces an instance");
    hs_string_free(json);

    char *out = NULL;
    status = hs_load_result(instance, &out);
    CHECK(status == HS_OK && out != NULL && strcmp(out, "Int(99)") == 0,
          "hs_load_result answers the entry module's own top-level return");
    hs_string_free(out);

    out = NULL;
    status = hs_load_result(instance, &out);
    CHECK(status == HS_REFUSED,
          "hs_load_result answers HS_REFUSED the second time it is called");
    hs_string_free(out);

    hs_dispose(instance);
}

static void test_version(void) {
    const char *version = hs_version();
    CHECK(version != NULL && version[0] != '\0', "hs_version answers a non-empty string");
}

static void test_capabilities(void) {
    captured_print[0] = '\0';
    int32_t print_params[1] = {HS_TYPE_STRING};
    int32_t square_params[1] = {HS_TYPE_INT};
    int32_t cache_params[1] = {HS_TYPE_STRING};
    HsCapabilityFn capabilities[5];
    capabilities[0].name = "print";
    capabilities[0].function = NULL;
    capabilities[0].params = print_params;
    capabilities[0].params_len = 1;
    capabilities[0].result_shape = HS_RESULT_NONE;
    capabilities[0].result = HS_TYPE_INT;
    capabilities[0].result_err = HS_TYPE_INT;
    capabilities[0].call = capture_print;
    capabilities[0].user_data = captured_print;
    capabilities[0].params_json = NULL;
    capabilities[0].result_json = NULL;
    capabilities[1].name = "square";
    capabilities[1].function = NULL;
    capabilities[1].params = square_params;
    capabilities[1].params_len = 1;
    capabilities[1].result_shape = HS_RESULT_VALUE;
    capabilities[1].result = HS_TYPE_INT;
    capabilities[1].result_err = HS_TYPE_INT;
    capabilities[1].call = square;
    capabilities[1].user_data = NULL;
    capabilities[1].params_json = NULL;
    capabilities[1].result_json = NULL;
    /* Two entries sharing the name "clock": a namespace capability. */
    capabilities[2].name = "clock";
    capabilities[2].function = "now";
    capabilities[2].params = NULL;
    capabilities[2].params_len = 0;
    capabilities[2].result_shape = HS_RESULT_VALUE;
    capabilities[2].result = HS_TYPE_STRING;
    capabilities[2].result_err = HS_TYPE_INT;
    capabilities[2].call = clock_now;
    capabilities[2].user_data = NULL;
    capabilities[2].params_json = NULL;
    capabilities[2].result_json = NULL;
    capabilities[3].name = "clock";
    capabilities[3].function = "today";
    capabilities[3].params = NULL;
    capabilities[3].params_len = 0;
    capabilities[3].result_shape = HS_RESULT_VALUE;
    capabilities[3].result = HS_TYPE_STRING;
    capabilities[3].result_err = HS_TYPE_INT;
    capabilities[3].call = clock_today;
    capabilities[3].user_data = NULL;
    capabilities[3].params_json = NULL;
    capabilities[3].result_json = NULL;
    /* A plain function capability declared Result<String, String>. */
    capabilities[4].name = "cache";
    capabilities[4].function = NULL;
    capabilities[4].params = cache_params;
    capabilities[4].params_len = 1;
    capabilities[4].result_shape = HS_RESULT_RESULT;
    capabilities[4].result = HS_TYPE_STRING;
    capabilities[4].result_err = HS_TYPE_STRING;
    capabilities[4].call = cache_get;
    capabilities[4].user_data = NULL;
    capabilities[4].params_json = NULL;
    capabilities[4].result_json = NULL;

    const char *source =
        "uses print\n"
        "uses square\n"
        "uses clock\n"
        "uses cache\n"
        "export constant answer = 42\n"
        "export fn run(): Int {\n"
        "    effect print(\"hello from C\")\n"
        "    return effect square(6)\n"
        "}\n"
        "export fn today(): String {\n"
        "    return effect clock.today()\n"
        "}\n"
        "export fn timestamp(): String {\n"
        "    return effect clock.now()\n"
        "}\n"
        "export fn lookup(key: String): String {\n"
        "    match effect cache(key) {\n"
        "        Ok(value) { return value }\n"
        "        Err(reason) { return \"missing: ${reason}\" }\n"
        "    }\n"
        "}\n";

    HsInstance *instance = NULL;
    char *json = NULL;
    int status = hs_load_ex(source, "main.hws", capabilities, 5, NULL, NULL,
                             &instance, &json);
    CHECK(status == HS_OK, "hs_load_ex loads a program with five capability entries");
    if (status != HS_OK && json != NULL) {
        fprintf(stderr, "diagnostics: %s\n", json);
    }
    CHECK(instance != NULL, "hs_load_ex produces an instance");
    hs_string_free(json);

    char *out = NULL;
    status = hs_call_ex(instance, "run", "[]", NULL, &out);
    CHECK(status == HS_OK, "run() succeeds");
    CHECK(out != NULL && strcmp(out, "Int(36)") == 0,
          "square(6) answered through the capability is 36");
    hs_string_free(out);

    CHECK(strstr(captured_print, "hello from C") != NULL,
          "the print capability captured the argument");

    out = NULL;
    status = hs_call_ex(instance, "today", "[]", NULL, &out);
    CHECK(status == HS_OK && out != NULL &&
              strcmp(out, "String(\"2026-09-23\")") == 0,
          "clock.today() answers through the namespace capability");
    hs_string_free(out);

    out = NULL;
    status = hs_call_ex(instance, "timestamp", "[]", NULL, &out);
    CHECK(status == HS_OK && out != NULL &&
              strcmp(out, "String(\"2026-09-23T12:00:00Z\")") == 0,
          "clock.now() answers its own function of the same namespace");
    hs_string_free(out);

    out = NULL;
    status = hs_call_ex(instance, "lookup", "[\"found-key\"]", NULL, &out);
    CHECK(status == HS_OK && out != NULL &&
              strcmp(out, "String(\"cached value\")") == 0,
          "cache(key) answers Ok(value), matched by the program");
    hs_string_free(out);

    out = NULL;
    status = hs_call_ex(instance, "lookup", "[\"missing-key\"]", NULL, &out);
    CHECK(status == HS_OK && out != NULL &&
              strcmp(out, "String(\"missing: no such key\")") == 0,
          "cache(key) answers Err(reason), matched by the program, not a fault");
    hs_string_free(out);

    out = NULL;
    status = hs_read(instance, "answer", &out);
    CHECK(status == HS_OK && out != NULL && strcmp(out, "Int(42)") == 0,
          "hs_read answers the exported constant");
    hs_string_free(out);

    hs_dispose(instance);
}

/* A capability answering a record: Studio's `entities.get`-shaped, the case
 * HsValueType's four scalars cannot declare on their own. */
static int32_t entities_get(void *user_data, const char *args_json,
                             char **out_json, char **out_error) {
    (void)user_data;
    (void)args_json;
    (void)out_error;
    *out_json = "{\"record\":{\"id\":\"player-1\",\"x\":3,\"y\":4}}";
    return 0;
}

/* A capability declaring its parameter and result types through
 * params_json/result_json (chapter 12's full transferable-type grammar,
 * documented above HsCapabilityFn in hollowscript.h), bringing the C ABI to
 * parity with hollowscript-wasm's own tooling boundary: a C host can now
 * declare a record, list, map, set, enum or optional parameter or result,
 * not only the four HsValueType scalars. */
static void test_full_type_declarations(void) {
    int32_t entities_params[1] = {HS_TYPE_STRING};
    HsCapabilityFn capabilities[1];
    capabilities[0].name = "entities";
    capabilities[0].function = "get";
    capabilities[0].params = entities_params;
    capabilities[0].params_len = 1;
    capabilities[0].result_shape = HS_RESULT_VALUE;
    capabilities[0].result = HS_TYPE_INT; /* unread: result_json wins */
    capabilities[0].result_err = HS_TYPE_INT;
    capabilities[0].call = entities_get;
    capabilities[0].user_data = NULL;
    capabilities[0].params_json = NULL;
    capabilities[0].result_json =
        "{\"record\":{\"id\":\"String\",\"x\":\"Int\",\"y\":\"Int\"}}";

    const char *source =
        "uses entities\n"
        "export fn run(): Int {\n"
        "    constant e = effect entities.get(\"player-1\")\n"
        "    return e.x + e.y\n"
        "}\n";

    HsInstance *instance = NULL;
    char *json = NULL;
    int status = hs_load_ex(source, "main.hws", capabilities, 1, NULL, NULL,
                             &instance, &json);
    CHECK(status == HS_OK,
          "hs_load_ex loads a program with a result_json-declared record capability");
    if (status != HS_OK && json != NULL) {
        fprintf(stderr, "diagnostics: %s\n", json);
    }
    CHECK(instance != NULL, "hs_load_ex produces an instance");
    hs_string_free(json);

    char *out = NULL;
    status = hs_call_ex(instance, "run", "[]", NULL, &out);
    CHECK(status == HS_OK && out != NULL && strcmp(out, "Int(7)") == 0,
          "run() reads the record fields entities.get answered");
    hs_string_free(out);

    hs_dispose(instance);

    /* A malformed result_json is HS_INVALID_CAPABILITY, with the reason in
     * *out_json, not a crash or a silent fallback to the scalar result. */
    capabilities[0].result_json = "not json";
    instance = NULL;
    json = NULL;
    status = hs_load_ex(source, "main.hws", capabilities, 1, NULL, NULL, &instance,
                         &json);
    CHECK(status == HS_INVALID_CAPABILITY,
          "a malformed result_json is HS_INVALID_CAPABILITY");
    CHECK(instance == NULL, "no instance is created for a malformed declaration");
    CHECK(json != NULL && json[0] != '\0',
          "the reason is written to *out_json");
    hs_string_free(json);
}

struct canceller_args {
    HsCancelToken *token;
};

static void *cancel_after_delay(void *arg) {
    struct canceller_args *args = (struct canceller_args *)arg;
    usleep(20 * 1000);
    hs_cancel_token_cancel(args->token);
    return NULL;
}

static void test_cancellation(void) {
    const char *source = "export fn spin() {\n    while true {\n    }\n}\n";
    HsInstance *instance = NULL;
    char *json = NULL;
    int status = hs_load(source, "main.hws", &instance, &json);
    CHECK(status == HS_OK, "hs_load loads the spinning program");
    hs_string_free(json);

    HsCancelToken *token = hs_cancel_token_new();
    struct canceller_args args = {token};
    pthread_t thread;
    CHECK(pthread_create(&thread, NULL, cancel_after_delay, &args) == 0,
          "the cancelling thread starts");

    char *out = NULL;
    status = hs_call_ex(instance, "spin", "[]", token, &out);
    pthread_join(thread, NULL);

    CHECK(status == HS_FAULTED, "the cancelled call faults");
    CHECK(out != NULL && strstr(out, "cancelled") != NULL,
          "the fault reports cancellation");
    hs_string_free(out);

    hs_cancel_token_free(token);
    hs_dispose(instance);
}

/* A capability call that frees the cancellation token it was itself passed,
 * from inside the callback, while hs_call_ex is still running on it: the
 * token must go on being read safely for the rest of that one call (it
 * clones the flag at entry rather than borrowing the host's own memory for
 * the call's whole length), so this must run clean, in particular under
 * AddressSanitizer, rather than reading freed memory. */
static HsCancelToken *token_to_free_mid_call;

static int32_t free_token_mid_call(void *user_data, const char *args_json,
                                    char **out_json, char **out_error) {
    (void)user_data;
    (void)args_json;
    (void)out_error;
    hs_cancel_token_free(token_to_free_mid_call);
    *out_json = "1";
    return 0;
}

static void test_token_freed_during_the_call_that_uses_it(void) {
    HsCapabilityFn capabilities[1];
    capabilities[0].name = "host";
    capabilities[0].function = NULL;
    capabilities[0].params = NULL;
    capabilities[0].params_len = 0;
    capabilities[0].result_shape = HS_RESULT_VALUE;
    capabilities[0].result = HS_TYPE_INT;
    capabilities[0].result_err = HS_TYPE_INT;
    capabilities[0].call = free_token_mid_call;
    capabilities[0].user_data = NULL;
    capabilities[0].params_json = NULL;
    capabilities[0].result_json = NULL;

    const char *source =
        "uses host\nexport fn f(): Int {\n    return effect host() + 1\n}\n";
    HsInstance *instance = NULL;
    char *json = NULL;
    int status = hs_load_ex(source, "main.hws", capabilities, 1, NULL, NULL,
                             &instance, &json);
    CHECK(status == HS_OK, "hs_load_ex loads the token-freeing program");
    hs_string_free(json);

    token_to_free_mid_call = hs_cancel_token_new();
    char *out = NULL;
    status = hs_call_ex(instance, "f", "[]", token_to_free_mid_call, &out);
    CHECK(status == HS_OK && out != NULL && strcmp(out, "Int(2)") == 0,
          "the call finishes correctly even though its own token was freed "
          "part way through it");
    hs_string_free(out);

    hs_dispose(instance);
}

/* A NULL capabilities array with a huge capabilities_len must be
 * HS_NULL_ARGUMENT, not an allocator abort: hs_load_ex must not pre-size
 * anything from a caller-supplied length before it has checked the pointer
 * that length describes. */
static void test_null_capabilities_with_huge_length(void) {
    HsInstance *instance = NULL;
    char *json = NULL;
    int status = hs_load_ex("return 1\n", "main.hws", NULL,
                             (size_t)1 << 40, NULL, NULL, &instance, &json);
    CHECK(status == HS_NULL_ARGUMENT,
          "a NULL capabilities array with a huge length is HS_NULL_ARGUMENT, "
          "not an abort");
    CHECK(instance == NULL, "no instance is created");
    hs_string_free(json);
}

static int32_t unreached(void *user_data, const char *args_json,
                          char **out_json, char **out_error) {
    (void)user_data;
    (void)args_json;
    (void)out_json;
    (void)out_error;
    return 0;
}

/* Out-of-range HsValueType/HsResultShape values, written as raw integers a
 * C caller could produce by mistake (or a stray non-boolean cast), must be
 * refused as HS_INVALID_CAPABILITY: the library never reads them as a Rust
 * enum whose discriminant they may not name. */
static void test_out_of_range_capability_enums(void) {
    int32_t bad_params[1] = {99};
    HsCapabilityFn capabilities[1];
    capabilities[0].name = "host";
    capabilities[0].function = NULL;
    capabilities[0].params = bad_params;
    capabilities[0].params_len = 1;
    capabilities[0].result_shape = HS_RESULT_NONE;
    capabilities[0].result = HS_TYPE_INT;
    capabilities[0].result_err = HS_TYPE_INT;
    capabilities[0].call = unreached;
    capabilities[0].user_data = NULL;
    capabilities[0].params_json = NULL;
    capabilities[0].result_json = NULL;

    HsInstance *instance = NULL;
    char *json = NULL;
    int status = hs_load_ex("return 1\n", "main.hws", capabilities, 1, NULL, NULL,
                             &instance, &json);
    CHECK(status == HS_INVALID_CAPABILITY,
          "an out-of-range params[0] value is HS_INVALID_CAPABILITY");
    CHECK(instance == NULL, "no instance is created");
    hs_string_free(json);

    capabilities[0].params = NULL;
    capabilities[0].params_len = 0;
    capabilities[0].result_shape = (int32_t)7;
    instance = NULL;
    json = NULL;
    status = hs_load_ex("return 1\n", "main.hws", capabilities, 1, NULL, NULL,
                         &instance, &json);
    CHECK(status == HS_INVALID_CAPABILITY,
          "an out-of-range result_shape value is HS_INVALID_CAPABILITY");
    CHECK(instance == NULL, "no instance is created");
    hs_string_free(json);
}

/* Every out_* pointer is left NULL, never untouched, on an error path that
 * runs before this library would otherwise have written a real answer:
 * invalid UTF-8, a NULL instance, and a NULL capabilities-with-length
 * mismatch all leave *out at whatever this test poisoned it with first. */
static void test_output_pointers_are_cleared_first(void) {
    char poison_buf[1];
    char *poison = poison_buf;

    char *out = poison;
    int status = hs_check_json("x\xff", NULL, &out);
    CHECK(status == HS_INVALID_UTF8, "hs_check_json refuses invalid UTF-8");
    CHECK(out == NULL, "hs_check_json clears *out_json on invalid UTF-8");

    out = poison;
    status = hs_load("x\xff", "main.hws", NULL, &out);
    /* out_instance is NULL here, so this is itself HS_NULL_ARGUMENT; the
     * point is that *out_json is still cleared before that check runs. */
    CHECK(out == NULL, "hs_load clears *out_json even when out_instance is NULL");

    HsInstance *inst = NULL;
    char *json = NULL;
    status = hs_load("x\xff", "main.hws", &inst, &json);
    CHECK(status == HS_INVALID_UTF8, "hs_load refuses invalid UTF-8");
    CHECK(inst == NULL, "hs_load clears *out_instance on invalid UTF-8");
    CHECK(json == NULL, "hs_load clears *out_json on invalid UTF-8");

    out = poison;
    status = hs_call(NULL, "f", "[]", &out);
    CHECK(status == HS_NULL_ARGUMENT, "hs_call(NULL, ...) is HS_NULL_ARGUMENT");
    CHECK(out == NULL, "hs_call clears *out_text even for a NULL instance");

    out = poison;
    status = hs_read(NULL, "\xff", &out);
    CHECK(status == HS_NULL_ARGUMENT, "hs_read(NULL, ...) is HS_NULL_ARGUMENT");
    CHECK(out == NULL, "hs_read clears *out_text even for a NULL instance");
}

int main(void) {
    test_version();
    test_capabilities();
    test_transferable_types();
    test_full_type_declarations();
    test_load_result();
    test_cancellation();
    test_token_freed_during_the_call_that_uses_it();
    test_null_capabilities_with_huge_length();
    test_out_of_range_capability_enums();
    test_output_pointers_are_cleared_first();
    if (failed) {
        fprintf(stderr, "one or more checks failed\n");
        return 1;
    }
    printf("all checks passed\n");
    return 0;
}

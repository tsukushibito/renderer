/* Benchmark-only Windows CPU boundaries for the pinned Godot 4.7.2 build.
 * No engine hooks, worker, polling, or per-frame sampling are installed.
 */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <stdint.h>
#include <string.h>

#include "vendor/gdextension_interface.h"

#if !defined(_WIN64) || !defined(_M_X64)
#error B3 CPU clock requires Windows x86_64.
#endif

#define GODOT_COMMIT "ed1daf0bf001b61586d9930840f2f1394092c079"

/* Standard Windows x86_64 builtin sizes, verified against the pinned editor's
 * --dump-extension-api by build.py. These are opaque storage, not engine types.
 */
typedef union { uint64_t alignment; unsigned char bytes[24]; } VariantStorage;
typedef union { uint64_t alignment; unsigned char bytes[8]; } NameStorage;

static GDExtensionClassLibraryPtr library;
static GDExtensionInterfacePrintError report_error;
static GDExtensionInterfaceStringNameNewWithUtf8Chars name_new;
static GDExtensionInterfaceStringNewWithUtf8Chars string_new;
static GDExtensionInterfaceClassdbRegisterExtensionClass6 register_class;
static GDExtensionInterfaceClassdbRegisterExtensionClassMethod register_method;
static GDExtensionInterfaceClassdbUnregisterExtensionClass unregister_class;
static GDExtensionInterfaceVariantConstruct variant_construct;
static GDExtensionInterfaceVariantCall variant_call;
static GDExtensionInterfaceVariantSetIndexed variant_set_indexed;
static GDExtensionInterfaceVariantDestroy variant_destroy;
static GDExtensionInterfaceVariantGetType variant_get_type;
static GDExtensionVariantFromTypeConstructorFunc int_to_variant;
static GDExtensionTypeFromVariantConstructorFunc variant_to_int;
static GDExtensionPtrDestructor name_destroy;
static GDExtensionPtrDestructor string_destroy;
static NameStorage class_name, object_name, method_name, resize_name, empty_name;

static int64_t file_ticks(FILETIME value) {
    return (int64_t)(((uint64_t)value.dwHighDateTime << 32) | value.dwLowDateTime);
}

/* Returns zero on success. Failure is an explicit negative stage in out[0],
 * with the Windows error in out[1]; callers must never treat it as a sample.
 * QPC -> frequency -> process -> caller-thread is the exact capture order.
 * FILETIME's 100 ns unit is not a claim of 100 ns accounting precision.
 */
__declspec(dllexport) int b3_cpu_clock_read(int64_t out[7]) {
    LARGE_INTEGER qpc, frequency;
    FILETIME created, exited, kernel, user;
    memset(out, 0, 7 * sizeof(*out));
    if (!QueryPerformanceCounter(&qpc)) { out[0] = -1; goto failed; }
    if (!QueryPerformanceFrequency(&frequency)) { out[0] = -2; goto failed; }
    if (qpc.QuadPart < 0 || frequency.QuadPart <= 0) {
        out[0] = -3; out[1] = ERROR_INVALID_DATA; return -3;
    }
    out[0] = qpc.QuadPart;
    out[1] = frequency.QuadPart;
    /* Pseudo handles are borrowed, never closed. Live-process/thread exit
     * values are undefined and are not read. */
    if (!GetProcessTimes(GetCurrentProcess(), &created, &exited, &kernel, &user)) {
        out[0] = -4; goto failed;
    }
    out[2] = file_ticks(kernel); out[3] = file_ticks(user);
    if (!GetThreadTimes(GetCurrentThread(), &created, &exited, &kernel, &user)) {
        out[0] = -5; goto failed;
    }
    out[4] = file_ticks(kernel); out[5] = file_ticks(user);
    out[6] = (int64_t)GetCurrentThreadId();
    return 0;
failed:
    out[1] = (int64_t)GetLastError();
    return (int)out[0];
}

static void error(const char *message) {
    report_error(message, "B3CpuClock", "b3_cpu_clock.c", __LINE__, 0);
}

/* r_return is an initialized Godot Variant; all other Variant buffers passed
 * as constructor/call return values below are uninitialized as the ABI requires.
 * Packing can allocate twice per ROI. It is never called once per frame.
 */
static void snapshot_variant(GDExtensionVariantPtr r_return) {
    int64_t values[7], count = 7, resize_result = -1;
    VariantStorage argument, returned;
    GDExtensionConstVariantPtr args[1] = { &argument };
    GDExtensionCallError call_error = { GDEXTENSION_CALL_OK, 0, 0 };
    int status = b3_cpu_clock_read(values);
    if (status != 0) { error("Windows CPU boundary capture failed; negative snapshot is invalid."); }
    variant_destroy(r_return);
    variant_construct(GDEXTENSION_VARIANT_TYPE_PACKED_INT64_ARRAY, r_return, NULL, 0, &call_error);
    if (call_error.error != GDEXTENSION_CALL_OK) { error("Cannot construct CPU snapshot array."); return; }
    int_to_variant(&argument, &count);
    variant_call(r_return, &resize_name, args, 1, &returned, &call_error);
    if (call_error.error == GDEXTENSION_CALL_OK && variant_get_type(&returned) == GDEXTENSION_VARIANT_TYPE_INT) {
        variant_to_int(&resize_result, &returned);
    }
    variant_destroy(&returned);
    variant_destroy(&argument);
    if (call_error.error != GDEXTENSION_CALL_OK || resize_result != 0) {
        error("Cannot resize CPU snapshot array."); return;
    }
    for (int64_t i = 0; i < 7; ++i) {
        GDExtensionBool valid = 0, out_of_bounds = 1;
        int_to_variant(&argument, &values[i]);
        variant_set_indexed(r_return, i, &argument, &valid, &out_of_bounds);
        variant_destroy(&argument);
        if (!valid || out_of_bounds) { error("Cannot store CPU snapshot field."); return; }
    }
}

static void snapshot_call(void *userdata, GDExtensionClassInstancePtr instance,
        const GDExtensionConstVariantPtr *args, GDExtensionInt count,
        GDExtensionVariantPtr r_return, GDExtensionCallError *call_error) {
    (void)userdata; (void)instance; (void)args;
    if (count != 0) {
        call_error->error = GDEXTENSION_CALL_ERROR_TOO_MANY_ARGUMENTS;
        call_error->argument = 0; call_error->expected = 0; return;
    }
    snapshot_variant(r_return);
    call_error->error = GDEXTENSION_CALL_OK;
}

static void unused_free(void *userdata, GDExtensionClassInstancePtr instance) {
    (void)userdata; (void)instance;
}

static void initialize(void *userdata, GDExtensionInitializationLevel level) {
    GDExtensionClassCreationInfo6 creation = { 0 };
    GDExtensionClassMethodInfo method = { 0 };
    GDExtensionPropertyInfo result = { 0 };
    NameStorage empty_string;
    (void)userdata;
    if (level != GDEXTENSION_INITIALIZATION_SCENE) { return; }
    name_new(&class_name, "B3CpuClock"); name_new(&object_name, "Object");
    name_new(&method_name, "snapshot"); name_new(&resize_name, "resize"); name_new(&empty_name, "");
    string_new(&empty_string, "");
    creation.is_abstract = 1; creation.is_exposed = 1; creation.free_instance_func = unused_free;
    register_class(library, &class_name, &object_name, &creation);
    result.type = GDEXTENSION_VARIANT_TYPE_PACKED_INT64_ARRAY;
    result.name = &empty_name; result.class_name = &empty_name; result.hint_string = &empty_string;
    method.name = &method_name; method.call_func = snapshot_call;
    /* Variant-call only: avoids a separate typed return-storage ABI. The
     * callback rejects arguments; VARARG prevents Godot selecting ptrcall. */
    method.method_flags = GDEXTENSION_METHOD_FLAG_NORMAL | GDEXTENSION_METHOD_FLAG_STATIC | GDEXTENSION_METHOD_FLAG_VARARG;
    method.has_return_value = 1; method.return_value_info = &result;
    register_method(library, &class_name, &method);
    string_destroy(&empty_string);
}

static void deinitialize(void *userdata, GDExtensionInitializationLevel level) {
    (void)userdata;
    if (level != GDEXTENSION_INITIALIZATION_SCENE) { return; }
    unregister_class(library, &class_name);
    name_destroy(&empty_name); name_destroy(&resize_name); name_destroy(&method_name);
    name_destroy(&object_name); name_destroy(&class_name);
}

#define LOAD(name, type, symbol) do { name = (type)get_proc_address(symbol); if (!name) { return 0; } } while (0)

__declspec(dllexport) GDExtensionBool b3_cpu_clock_init(GDExtensionInterfaceGetProcAddress get_proc_address,
        GDExtensionClassLibraryPtr p_library, GDExtensionInitialization *initialization) {
    GDExtensionInterfaceGetGodotVersion2 get_version;
    GDExtensionInterfaceGetVariantFromTypeConstructor from_type;
    GDExtensionInterfaceGetVariantToTypeConstructor to_type;
    GDExtensionInterfaceVariantGetPtrDestructor get_destructor;
    GDExtensionGodotVersion2 version;
    LOAD(report_error, GDExtensionInterfacePrintError, "print_error");
    LOAD(get_version, GDExtensionInterfaceGetGodotVersion2, "get_godot_version2");
    get_version(&version);
    if (version.major != 4 || version.minor != 7 || version.patch != 2 ||
            !version.hash || strcmp(version.hash, GODOT_COMMIT) != 0 ||
            !version.status || strcmp(version.status, "stable") != 0 ||
            !version.build || strcmp(version.build, "official") != 0 ||
            !version.string || strstr(version.string, "double") != NULL) {
        error("B3CpuClock requires the pinned official standard Godot 4.7.2 build."); return 0;
    }
    LOAD(name_new, GDExtensionInterfaceStringNameNewWithUtf8Chars, "string_name_new_with_utf8_chars");
    LOAD(string_new, GDExtensionInterfaceStringNewWithUtf8Chars, "string_new_with_utf8_chars");
    LOAD(register_class, GDExtensionInterfaceClassdbRegisterExtensionClass6, "classdb_register_extension_class6");
    LOAD(register_method, GDExtensionInterfaceClassdbRegisterExtensionClassMethod, "classdb_register_extension_class_method");
    LOAD(unregister_class, GDExtensionInterfaceClassdbUnregisterExtensionClass, "classdb_unregister_extension_class");
    LOAD(variant_construct, GDExtensionInterfaceVariantConstruct, "variant_construct");
    LOAD(variant_call, GDExtensionInterfaceVariantCall, "variant_call");
    LOAD(variant_set_indexed, GDExtensionInterfaceVariantSetIndexed, "variant_set_indexed");
    LOAD(variant_destroy, GDExtensionInterfaceVariantDestroy, "variant_destroy");
    LOAD(variant_get_type, GDExtensionInterfaceVariantGetType, "variant_get_type");
    LOAD(from_type, GDExtensionInterfaceGetVariantFromTypeConstructor, "get_variant_from_type_constructor");
    LOAD(to_type, GDExtensionInterfaceGetVariantToTypeConstructor, "get_variant_to_type_constructor");
    LOAD(get_destructor, GDExtensionInterfaceVariantGetPtrDestructor, "variant_get_ptr_destructor");
    int_to_variant = from_type(GDEXTENSION_VARIANT_TYPE_INT);
    variant_to_int = to_type(GDEXTENSION_VARIANT_TYPE_INT);
    name_destroy = get_destructor(GDEXTENSION_VARIANT_TYPE_STRING_NAME);
    string_destroy = get_destructor(GDEXTENSION_VARIANT_TYPE_STRING);
    if (!int_to_variant || !variant_to_int || !name_destroy || !string_destroy) {
        error("B3CpuClock required builtin constructors are unavailable."); return 0;
    }
    library = p_library;
    initialization->minimum_initialization_level = GDEXTENSION_INITIALIZATION_SCENE;
    initialization->userdata = NULL; initialization->initialize = initialize; initialization->deinitialize = deinitialize;
    return 1;
}

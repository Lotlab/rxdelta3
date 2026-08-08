// wrap_test.exe — load XDelta3WrapFactory.dll and run a full MergeDirCustomDiffV2
// against a real patch set.
//
// Arguments:
//   --src    <dir>   source tree root (required)
//   --patch  <dir>   patch dir holding patch_delta_direct.dat + Pkg\ (required)
//   --dst    <dir>   output dir. Default: auto-create .\out_<timestamp>
//   --dll    <path>  DLL to load (default: XDelta3WrapFactory.dll in exe dir)
//   --mergecb self|none   mergeCb = GetProcAddress("MergeFile") or NULL (default none)
//
// The output directory is always created fresh and never overwrites existing
// files (an explicit --dst that already exists is rejected).

#include <windows.h>
#include <stdio.h>
#include <wchar.h>
#include <string.h>
#include <time.h>

typedef BOOL (__stdcall *MergeDirCustomDiffV2_t)(
    const wchar_t*, const wchar_t*, const wchar_t*, void*, void*, void*);

static int file_events = 0;
static int cmd_events = 0;
static int last_done = 0, last_total = 0;
static int saw_error = 0;
static LARGE_INTEGER t_start, t_prev;
static LARGE_INTEGER freq;
static int have_freq = 0;

// Milliseconds since test start.
static double ms_since(LARGE_INTEGER* now) {
    if (!have_freq) return 0;
    return (double)(now->QuadPart - t_start.QuadPart) * 1000.0 / freq.QuadPart;
}
// Milliseconds between prev and now, then update prev.
static double ms_delta(LARGE_INTEGER* now) {
    if (!have_freq) return 0;
    double d = (double)(now->QuadPart - t_prev.QuadPart) * 1000.0 / freq.QuadPart;
    t_prev = *now;
    return d;
}

// Print a wide string to the console (UTF-16) with a UTF-8 fallback for
// redirected output.
static void print_wide(const wchar_t* s) {
    HANDLE h = GetStdHandle(STD_OUTPUT_HANDLE);
    DWORD written = 0;
    if (h != INVALID_HANDLE_VALUE && h != NULL && GetFileType(h) == FILE_TYPE_CHAR) {
        WriteConsoleW(h, s, (DWORD)wcslen(s), &written, NULL);
        WriteConsoleW(h, L"\n", 1, &written, NULL);
        return;
    }
    // Redirected: convert to UTF-8.
    int n = WideCharToMultiByte(CP_UTF8, 0, s, -1, NULL, 0, NULL, NULL);
    if (n > 0) {
        char* buf = malloc(n);
        WideCharToMultiByte(CP_UTF8, 0, s, -1, buf, n, NULL, NULL);
        fputs(buf, stdout);
        fputc('\n', stdout);
        free(buf);
    }
}

static void __stdcall file_cb(const wchar_t* msg, int ty) {
    LARGE_INTEGER now;
    QueryPerformanceCounter(&now);
    file_events++;
    if (ty != 0) saw_error = 1;
    printf("[file %3d ty=%d +%7.1fms] ", file_events, ty, ms_delta(&now));
    print_wide(msg);
    fflush(stdout);
}

static void __stdcall cmd_cb(int done, int total) {
    LARGE_INTEGER now;
    QueryPerformanceCounter(&now);
    cmd_events++;
    last_done = done;
    last_total = total;
    if (total > 0) {
        printf("[cmd] %d/%d (%.0f%%) +%7.1fms\n", done, total,
               100.0 * done / total, ms_delta(&now));
    } else {
        printf("[cmd] %d/%d +%7.1fms\n", done, total, ms_delta(&now));
    }
    fflush(stdout);
}

static const wchar_t* find_arg(int argc, wchar_t** argv, const wchar_t* name) {
    for (int i = 1; i + 1 < argc; i++) {
        if (_wcsicmp(argv[i], name) == 0) return argv[i + 1];
    }
    return NULL;
}

static int has_arg(int argc, wchar_t** argv, const wchar_t* name) {
    for (int i = 1; i < argc; i++) {
        if (_wcsicmp(argv[i], name) == 0) return 1;
    }
    return 0;
}

// Build .\out_<YYYYMMDD_HHMMSS>
static void make_default_dst(wchar_t* buf, int cap) {
    SYSTEMTIME st;
    GetLocalTime(&st);
    _snwprintf(buf, cap, L".\\out_%04d%02d%02d_%02d%02d%02d",
               st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond);
}

int wmain(int argc, wchar_t** argv) {
    if (has_arg(argc, argv, L"--help") || has_arg(argc, argv, L"-h")) {
        printf(
            "usage: wrap_test.exe [--src <dir>] [--patch <dir>] [--dst <dir>]\n"
            "                     [--dll <path>] [--mergecb self|none]\n");
        return 2;
    }

    const wchar_t* src = find_arg(argc, argv, L"--src");
    const wchar_t* patch = find_arg(argc, argv, L"--patch");
    const wchar_t* dst = find_arg(argc, argv, L"--dst");
    const wchar_t* dll = find_arg(argc, argv, L"--dll");
    const wchar_t* mergecb_mode = find_arg(argc, argv, L"--mergecb");

    wchar_t dst_buf[MAX_PATH], dll_buf[MAX_PATH];
    if (!src) {
        wprintf(L"FATAL: --src <dir> is required\n");
        return 1;
    }
    if (!patch) {
        wprintf(L"FATAL: --patch <dir> is required\n");
        return 1;
    }
    if (!dst) {
        make_default_dst(dst_buf, MAX_PATH);
        dst = dst_buf;
    }
    if (!dll) {
        GetModuleFileNameW(NULL, dll_buf, MAX_PATH);
        wchar_t* slash = wcsrchr(dll_buf, L'\\');
        if (slash) *(slash + 1) = 0;
        wcscat(dll_buf, L"XDelta3WrapFactory.dll");
        dll = dll_buf;
    }
    int mergecb_self = mergecb_mode && _wcsicmp(mergecb_mode, L"self") == 0;

    wprintf(L"=== XDelta3 wrap test ===\n");
    wprintf(L"  src   : %ls\n", src);
    wprintf(L"  patch : %ls\n", patch);
    wprintf(L"  dst   : %ls\n", dst);
    wprintf(L"  dll   : %ls\n", dll);
    wprintf(L"  mergecb: %s\n", mergecb_self ? L"self (GetProcAddress MergeFile)" : L"none (NULL)");

    // dst must not exist (never overwrite existing files).
    if (GetFileAttributesW(dst) != INVALID_FILE_ATTRIBUTES) {
        wprintf(L"FATAL: output directory already exists: %ls\n", dst);
        return 1;
    }
    if (!CreateDirectoryW(dst, NULL)) {
        wprintf(L"FATAL: cannot create output dir (error %lu)\n", GetLastError());
        return 1;
    }

    HMODULE h = LoadLibraryW(dll);
    if (!h) {
        wprintf(L"FATAL: LoadLibrary failed (error %lu)\n", GetLastError());
        return 1;
    }
    wprintf(L"Loaded %ls\n", dll);

    MergeDirCustomDiffV2_t pMergeV2 =
        (MergeDirCustomDiffV2_t)GetProcAddress(h, "MergeDirCustomDiffV2");
    if (!pMergeV2) {
        wprintf(L"FATAL: MergeDirCustomDiffV2 not found\n");
        FreeLibrary(h);
        return 1;
    }

    void* mergecb = NULL;
    if (mergecb_self) {
        mergecb = (void*)GetProcAddress(h, "MergeFile");
        if (!mergecb) {
            wprintf(L"FATAL: MergeFile not found\n");
            FreeLibrary(h);
            return 1;
        }
        wprintf(L"mergeCb = self MergeFile (%p)\n", mergecb);
    }

    LARGE_INTEGER t0, t1;
    QueryPerformanceFrequency(&freq);
    QueryPerformanceCounter(&t0);
    t_start = t0;
    t_prev = t0;
    have_freq = 1;

    wprintf(L"--- running MergeDirCustomDiffV2 ... (35GB tree copy + 31 patches) ---\n");
    fflush(stdout);
    BOOL ok = pMergeV2(src, patch, dst, (void*)file_cb, (void*)cmd_cb, mergecb);

    QueryPerformanceCounter(&t1);
    double secs = (double)(t1.QuadPart - t0.QuadPart) / freq.QuadPart;

    wprintf(L"--- result ---\n");
    wprintf(L"  success     : %s\n", ok ? L"TRUE" : L"FALSE");
    wprintf(L"  file events : %d\n", file_events);
    wprintf(L"  cmd events  : %d (last %d/%d)\n", cmd_events, last_done, last_total);
    wprintf(L"  saw errors  : %s\n", saw_error ? L"YES" : L"no");
    wprintf(L"  elapsed     : %.1f s\n", secs);
    wprintf(L"  output dir  : %ls\n", dst);

    FreeLibrary(h);
    return ok ? 0 : 1;
}

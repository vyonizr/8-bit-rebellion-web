/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
#define UNICODE
#define _UNICODE
#include <windows.h>
#include <commctrl.h>
#include "SDL_syswm.h"
#include "host_settings.h"

typedef struct {
    HWND choices[SETTINGS_ROWS];
    HFONT font;
    int done, applied, dpi;
} SettingsDialog;
static int px(SettingsDialog *dialog, int value) { return MulDiv(value, dialog->dpi, 96); }
static HWND control(HWND parent, SettingsDialog *dialog, const wchar_t *kind,
                    const wchar_t *text, DWORD style, int x, int y, int w, int h, int id) {
    HWND view = CreateWindowExW(0, kind, text, WS_CHILD | WS_VISIBLE | style,
        px(dialog, x), px(dialog, y), px(dialog, w), px(dialog, h), parent,
        (HMENU)(INT_PTR)id, GetModuleHandleW(NULL), NULL);
    SendMessageW(view, WM_SETFONT, (WPARAM)dialog->font, TRUE);
    return view;
}
static void wide(const char *text, wchar_t *buffer, int count) {
    MultiByteToWideChar(CP_UTF8, 0, text, -1, buffer, count);
}
static LRESULT CALLBACK settings_proc(HWND window, UINT message, WPARAM w, LPARAM l) {
    SettingsDialog *dialog = (SettingsDialog *)GetWindowLongPtrW(window, GWLP_USERDATA);
    if (message == WM_NCCREATE) {
        dialog = (SettingsDialog *)((CREATESTRUCTW *)l)->lpCreateParams;
        SetWindowLongPtrW(window, GWLP_USERDATA, (LONG_PTR)dialog);
    }
    if (dialog && (message == WM_CLOSE || (message == WM_COMMAND &&
        (LOWORD(w) == IDCANCEL || LOWORD(w) == IDOK)))) {
        dialog->applied = message == WM_COMMAND && LOWORD(w) == IDOK;
        dialog->done = 1;
        return 0;
    }
    return DefWindowProcW(window, message, w, l);
}
int touchHLE_show_settings(SDL_Window *window, HostSettings *settings) {
    SDL_SysWMinfo info;
    SDL_VERSION(&info.version);
    if (!SDL_GetWindowWMInfo(window, &info) || info.subsystem != SDL_SYSWM_WINDOWS) return -1;
    HWND owner = info.info.win.window;
    SettingsDialog dialog = {0};
    HDC dc = GetDC(owner);
    dialog.dpi = GetDeviceCaps(dc, LOGPIXELSX);
    ReleaseDC(owner, dc);
    dialog.font = CreateFontW(-MulDiv(10, dialog.dpi, 72), 0, 0, 0, FW_NORMAL,
        FALSE, FALSE, FALSE, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY, DEFAULT_PITCH, L"Segoe UI");
    WNDCLASSW cls = {0};
    cls.lpfnWndProc = settings_proc;
    cls.hInstance = GetModuleHandleW(NULL);
    cls.hCursor = LoadCursorW(NULL, IDC_ARROW);
    cls.hbrBackground = (HBRUSH)(COLOR_BTNFACE + 1);
    cls.lpszClassName = L"touchHLESettings";
    if (!RegisterClassW(&cls) && GetLastError() != ERROR_CLASS_ALREADY_EXISTS) {
        DeleteObject(dialog.font); return -1;
    }
    RECT frame = {0, 0, px(&dialog, 520), px(&dialog, 378)}, parent;
    DWORD style = WS_CAPTION | WS_SYSMENU;
    AdjustWindowRectEx(&frame, style, FALSE, WS_EX_DLGMODALFRAME);
    GetWindowRect(owner, &parent);
    int width = frame.right - frame.left, height = frame.bottom - frame.top;
    HWND panel = CreateWindowExW(WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT, cls.lpszClassName,
        L"Settings", style, (parent.left + parent.right - width) / 2,
        (parent.top + parent.bottom - height) / 2, width, height, owner, NULL, cls.hInstance, &dialog);
    if (!panel) { DeleteObject(dialog.font); return -1; }
    INITCOMMONCONTROLSEX controls = {sizeof(controls), ICC_WIN95_CLASSES};
    InitCommonControlsEx(&controls);
    HWND tooltip = CreateWindowExW(WS_EX_TOPMOST, TOOLTIPS_CLASSW, NULL,
        WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX, CW_USEDEFAULT, CW_USEDEFAULT,
        CW_USEDEFAULT, CW_USEDEFAULT, panel, NULL, cls.hInstance, NULL);
    SendMessageW(tooltip, TTM_SETMAXTIPWIDTH, 0, px(&dialog, 360));
    wchar_t help_text[SETTINGS_ROWS][640];
    control(panel, &dialog, L"STATIC", L"Video", 0, 24, 22, 472, 22, 0);
    control(panel, &dialog, L"STATIC", L"Audio", 0, 24, 232, 472, 22, 0);
    for (int i = 0; i < SETTINGS_ROWS; i++) {
        int y = i == 4 ? 270 : 60 + 42 * i;
        wchar_t text[128]; wide(settings_rows[i].label, text, 128);
        control(panel, &dialog, L"STATIC", text, 0, 24, y + 4, 168, 24, 0);
        if (settings_rows[i].help && settings_rows[i].help[0]) {
            HDC label_dc = GetDC(panel);
            HGDIOBJ old_font = SelectObject(label_dc, dialog.font);
            SIZE text_size; GetTextExtentPoint32W(label_dc, text, lstrlenW(text), &text_size);
            SelectObject(label_dc, old_font); ReleaseDC(panel, label_dc);
            HWND help = control(panel, &dialog, L"STATIC", L"\x24D8", SS_NOTIFY,
                24 + MulDiv(text_size.cx, 96, dialog.dpi) + 8, y + 2, 24, 26, 0);
            wide(settings_rows[i].help, help_text[i], 640);
            TOOLINFOW tool = {0}; tool.cbSize = sizeof(tool);
            tool.uFlags = TTF_IDISHWND | TTF_SUBCLASS;
            tool.hwnd = panel; tool.uId = (UINT_PTR)help; tool.lpszText = help_text[i];
            SendMessageW(tooltip, TTM_ADDTOOLW, 0, (LPARAM)&tool);
        }
        HWND choice = control(panel, &dialog, L"COMBOBOX", L"", CBS_DROPDOWNLIST | WS_TABSTOP | WS_VSCROLL,
            238, y, 258, 230, 100 + i);
        for (int j = 0; j < settings_rows[i].count; j++) {
            wide(settings_rows[i].choices[j], text, 128);
            SendMessageW(choice, CB_ADDSTRING, 0, (LPARAM)text);
        }
        SendMessageW(choice, CB_SETCURSEL, settings->indices[i], 0);
        dialog.choices[i] = choice;
    }
    control(panel, &dialog, L"STATIC", L"", SS_ETCHEDHORZ, 24, 318, 472, 1, 0);
    control(panel, &dialog, L"BUTTON", L"Cancel", BS_PUSHBUTTON | WS_TABSTOP, 306, 334, 90, 28, IDCANCEL);
    control(panel, &dialog, L"BUTTON", L"Apply", BS_DEFPUSHBUTTON | WS_TABSTOP, 406, 334, 90, 28, IDOK);
    EnableWindow(owner, FALSE);
    ShowWindow(panel, SW_SHOW); SetFocus(dialog.choices[0]);
    MSG event; int quit = 0;
    while (!dialog.done) {
        int result = GetMessageW(&event, NULL, 0, 0);
        if (result <= 0) { quit = result == 0; break; }
        if (!IsDialogMessageW(panel, &event)) { TranslateMessage(&event); DispatchMessageW(&event); }
    }
    if (dialog.applied) {
        for (int i = 0; i < SETTINGS_ROWS; i++) settings->indices[i] = (int)SendMessageW(dialog.choices[i], CB_GETCURSEL, 0, 0);
    }
    EnableWindow(owner, TRUE);
    DestroyWindow(panel); DeleteObject(dialog.font); SetForegroundWindow(owner);
    if (quit) PostQuitMessage((int)event.wParam);
    return dialog.applied;
}

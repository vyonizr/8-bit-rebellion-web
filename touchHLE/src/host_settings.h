/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
/* Native host controls only; no guest assets or guest settings are used. */
#include <stddef.h>
#define SETTINGS_ROWS 5
typedef struct { int indices[SETTINGS_ROWS]; } HostSettings;
typedef struct {
    const char *label;
    const char *help;
    const char *choices[6];
    int count;
} SettingsRow;
static const SettingsRow settings_rows[SETTINGS_ROWS] = {
    {"Resolution", "Output size. The original game proportions are preserved; wide output has side borders. Fullscreen uses the desktop display with a centered output area when it fits.",
     {"480 x 320 (Original)", "960 x 640", "1280 x 720 (HD)", "1920 x 1080 (Full HD)", "2560 x 1440 (QHD)", "3840 x 2160 (4K)"}, 6},
    {"Window mode", NULL,
     {"Windowed", "Fullscreen"}, 2},
    {"Render quality", "Internal rendering scale. Higher values increase GPU work. At 2x-4x, original texture pixels stay distinct to avoid seams between packed images; the picture is more pixelated.",
     {"1x (Original)", "2x", "3x", "4x"}, 4},
    {"Smooth scrolling", "On: camera, character movement and world markers target 60 FPS. Game logic and sprite frame changes stay at the original 15 FPS. Title, menus and idle locations stay at 15 FPS. Adds up to one original tick (about 67 ms) of visual delay. Off restores original rendering.",
     {"Off", "On"}, 2},
    {"Sound", NULL,
     {"On", "Muted"}, 2},
};

/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
#import <Cocoa/Cocoa.h>
#include "SDL_syswm.h"
#include "host_settings.h"

/* AppKit's delayed toolTip windows do not reliably appear in SDL's modal
 * run loop. Use explicit hover tracking and a nonactivating child panel. */
@interface THSettingsInfo : NSView
@property(nonatomic, copy) NSString *helpText;
@property(nonatomic, strong) NSTrackingArea *hoverArea;
@property(nonatomic, strong) NSPanel *helpPanel;
- (void)hideHelp;
@end
@implementation THSettingsInfo
- (void)drawRect:(NSRect)dirty {
    [@"ⓘ" drawInRect:NSMakeRect(3, 1, 22, 22) withAttributes:@{
        NSFontAttributeName: [NSFont systemFontOfSize:17],
        NSForegroundColorAttributeName: [NSColor secondaryLabelColor]}];
}
- (void)updateTrackingAreas {
    if (self.hoverArea) [self removeTrackingArea:self.hoverArea];
    self.hoverArea = [[NSTrackingArea alloc] initWithRect:NSZeroRect
        options:NSTrackingMouseEnteredAndExited | NSTrackingMouseMoved |
            NSTrackingActiveAlways | NSTrackingInVisibleRect owner:self userInfo:nil];
    [self addTrackingArea:self.hoverArea];
    [super updateTrackingAreas];
}
- (void)showHelp {
    if (self.helpPanel || !self.window || self.helpText.length == 0) return;
    for (NSView *other in self.superview.subviews) {
        if (other != self && [other isKindOfClass:[THSettingsInfo class]]) [(THSettingsInfo *)other hideHelp];
    }
    NSFont *font = [NSFont systemFontOfSize:13];
    CGFloat width = 340;
    NSRect textBounds = [self.helpText boundingRectWithSize:NSMakeSize(width, CGFLOAT_MAX)
        options:NSStringDrawingUsesLineFragmentOrigin | NSStringDrawingUsesFontLeading
        attributes:@{NSFontAttributeName: font}];
    CGFloat height = ceil(textBounds.size.height) + 4;
    NSPanel *hint = [[NSPanel alloc] initWithContentRect:NSMakeRect(0, 0, width + 24, height + 24)
        styleMask:NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel
        backing:NSBackingStoreBuffered defer:NO];
    hint.releasedWhenClosed = NO;
    hint.worksWhenModal = YES;
    hint.ignoresMouseEvents = YES;
    hint.hasShadow = YES;
    hint.animationBehavior = NSWindowAnimationBehaviorNone;
    hint.contentView.wantsLayer = YES;
    hint.contentView.layer.cornerRadius = 8;
    hint.contentView.layer.masksToBounds = YES;
    hint.contentView.layer.borderWidth = 1;
    hint.contentView.layer.borderColor = [NSColor separatorColor].CGColor;
    NSTextField *text = [NSTextField wrappingLabelWithString:self.helpText];
    text.font = font;
    text.frame = NSMakeRect(12, 12, width, height);
    [hint.contentView addSubview:text];
    NSRect icon = [self.window convertRectToScreen:[self convertRect:self.bounds toView:nil]];
    NSRect screen = self.window.screen.visibleFrame;
    CGFloat x = MAX(NSMinX(screen), MIN(NSMinX(icon), NSMaxX(screen) - NSWidth(hint.frame)));
    CGFloat y = NSMaxY(icon) + 8;
    if (y + NSHeight(hint.frame) > NSMaxY(screen)) y = NSMinY(icon) - NSHeight(hint.frame) - 8;
    [hint setFrameOrigin:NSMakePoint(x, MAX(NSMinY(screen), y))];
    self.helpPanel = hint;
    [self.window addChildWindow:hint ordered:NSWindowAbove];
    [hint orderFront:nil];
}
- (void)hideHelp {
    NSPanel *hint = self.helpPanel;
    if (!hint) return;
    [hint.parentWindow removeChildWindow:hint];
    [hint orderOut:nil];
    self.helpPanel = nil;
}
- (void)mouseEntered:(NSEvent *)event { [self showHelp]; }
- (void)mouseMoved:(NSEvent *)event { [self showHelp]; }
- (void)mouseExited:(NSEvent *)event { [self hideHelp]; }
- (void)mouseDown:(NSEvent *)event { [self showHelp]; }
- (void)viewWillMoveToWindow:(NSWindow *)window {
    [self hideHelp];
    [super viewWillMoveToWindow:window];
}
@end

@interface THSettingsController : NSObject <NSWindowDelegate>
@end
@implementation THSettingsController
- (void)cancel:(id)sender { [NSApp stopModalWithCode:NSModalResponseCancel]; }
- (void)apply:(id)sender { [NSApp stopModalWithCode:NSModalResponseOK]; }
- (BOOL)windowShouldClose:(NSWindow *)sender {
    [self cancel:nil];
    return NO;
}
@end

static NSTextField *label(NSString *text, NSRect frame, BOOL heading) {
    NSTextField *view = [NSTextField labelWithString:text];
    view.frame = frame;
    view.font = heading ? [NSFont boldSystemFontOfSize:14] : [NSFont systemFontOfSize:13];
    return view;
}

/* Called on the SDL parent/main stack; output changes only on Apply. */
int touchHLE_show_settings(SDL_Window *window, HostSettings *settings) {
    @autoreleasepool {
        SDL_SysWMinfo info;
        SDL_VERSION(&info.version);
        if (!SDL_GetWindowWMInfo(window, &info) || info.subsystem != SDL_SYSWM_COCOA) return -1;
        NSWindow *owner = info.info.cocoa.window;
        NSPanel *panel = [[NSPanel alloc] initWithContentRect:NSMakeRect(0, 0, 520, 378)
            styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable
            backing:NSBackingStoreBuffered defer:NO];
        panel.title = @"Settings";
        panel.releasedWhenClosed = NO;
        panel.hidesOnDeactivate = NO;
        panel.collectionBehavior = NSWindowCollectionBehaviorFullScreenAuxiliary;
        THSettingsController *controller = [THSettingsController new];
        panel.delegate = controller;
        NSView *content = panel.contentView;
        [content addSubview:label(@"Video", NSMakeRect(24, 334, 460, 22), YES)];
        NSMutableArray<NSPopUpButton *> *choices = [NSMutableArray new];
        NSMutableArray<THSettingsInfo *> *information = [NSMutableArray new];
        for (int i = 0; i < SETTINGS_ROWS; i++) {
            if (i == 4) [content addSubview:label(@"Audio", NSMakeRect(24, 126, 460, 22), YES)];
            CGFloat y = i == 4 ? 86 : 292 - 42 * i;
            NSString *name = [NSString stringWithUTF8String:settings_rows[i].label];
            NSTextField *nameView = label(name, NSMakeRect(24, y + 3, 180, 22), NO);
            [content addSubview:nameView];
            if (settings_rows[i].help && settings_rows[i].help[0]) {
                THSettingsInfo *help = [[THSettingsInfo alloc] initWithFrame:NSMakeRect(24 + [name sizeWithAttributes:@{NSFontAttributeName: nameView.font}].width + 8, y + 2, 24, 24)];
                help.helpText = [NSString stringWithUTF8String:settings_rows[i].help];
                help.accessibilityElement = YES;
                help.accessibilityRole = NSAccessibilityHelpTagRole;
                help.accessibilityLabel = [name stringByAppendingString:@" information"];
                help.accessibilityHelp = help.helpText;
                [content addSubview:help];
                [information addObject:help];
            }
            NSPopUpButton *choice = [[NSPopUpButton alloc] initWithFrame:NSMakeRect(238, y, 258, 28) pullsDown:NO];
            choice.accessibilityLabel = name;
            for (int j = 0; j < settings_rows[i].count; j++) {
                [choice addItemWithTitle:[NSString stringWithUTF8String:settings_rows[i].choices[j]]];
            }
            [choice selectItemAtIndex:settings->indices[i]];
            [content addSubview:choice];
            [choices addObject:choice];
        }
        NSBox *divider = [[NSBox alloc] initWithFrame:NSMakeRect(24, 60, 472, 1)];
        divider.boxType = NSBoxSeparator;
        [content addSubview:divider];
        NSButton *cancel = [NSButton buttonWithTitle:@"Cancel" target:controller action:@selector(cancel:)];
        cancel.frame = NSMakeRect(306, 16, 90, 32);
        cancel.keyEquivalent = @"\e";
        [content addSubview:cancel];
        NSButton *apply = [NSButton buttonWithTitle:@"Apply" target:controller action:@selector(apply:)];
        apply.frame = NSMakeRect(406, 16, 90, 32);
        apply.keyEquivalent = @"\r";
        [content addSubview:apply];
        for (int i = 0; i < SETTINGS_ROWS - 1; i++) choices[i].nextKeyView = choices[i + 1];
        choices.lastObject.nextKeyView = cancel;
        cancel.nextKeyView = apply;
        apply.nextKeyView = choices.firstObject;
        NSRect ownerFrame = owner.frame;
        [panel setFrameOrigin:NSMakePoint(NSMidX(ownerFrame) - 260, NSMidY(ownerFrame) - 189)];
        [owner addChildWindow:panel ordered:NSWindowAbove];
        [panel makeKeyAndOrderFront:nil];
        [panel makeFirstResponder:choices.firstObject];
        // Observe pointer events even when child panels disrupt mouseExited.
        // Event coordinates also work for synthesized accessibility input.
        panel.acceptsMouseMovedEvents = YES;
        id hover = [NSEvent addLocalMonitorForEventsMatchingMask:
            NSEventMaskMouseMoved | NSEventMaskLeftMouseDown | NSEventMaskLeftMouseUp |
            NSEventMaskLeftMouseDragged | NSEventMaskMouseEntered | NSEventMaskMouseExited
            handler:^NSEvent *(NSEvent *event) {
                if (event.window) {
                    NSPoint cursor = [event.window convertPointToScreen:event.locationInWindow];
                    for (THSettingsInfo *help in information) {
                        NSRect icon = [panel convertRectToScreen:[help convertRect:help.bounds toView:nil]];
                        if (NSPointInRect(cursor, icon)) [help showHelp];
                        else [help hideHelp];
                    }
                }
                return event;
            }];
        NSInteger result = [NSApp runModalForWindow:panel];
        [NSEvent removeMonitor:hover];
        if (result == NSModalResponseOK) {
            for (int i = 0; i < SETTINGS_ROWS; i++) settings->indices[i] = (int)choices[i].indexOfSelectedItem;
        }
        for (THSettingsInfo *help in information) [help hideHelp];
        [owner removeChildWindow:panel];
        [panel orderOut:nil];
        [owner makeKeyAndOrderFront:nil];
        return result == NSModalResponseOK ? 1 : 0;
    }
}

#import <Cocoa/Cocoa.h>
#import <QuartzCore/QuartzCore.h>
#import <sys/utsname.h>
#include <stdio.h>

static NSDictionary *ProbeRectJSON(NSRect r) {
    return @{ @"x": @(r.origin.x), @"y": @(r.origin.y),
              @"width": @(r.size.width), @"height": @(r.size.height) };
}

static NSDictionary *ViewState(NSView *v) {
    if (!v) return @{};
    return @{ @"frame": ProbeRectJSON(v.frame), @"bounds": ProbeRectJSON(v.bounds),
              @"visibleRect": ProbeRectJSON(v.visibleRect),
              @"rectInWindow": ProbeRectJSON([v convertRect:v.bounds toView:nil]),
              @"backingBounds": ProbeRectJSON([v convertRectToBacking:v.bounds]),
              @"layerFrame": v.layer ? ProbeRectJSON(v.layer.frame) : @{},
              @"layerBounds": v.layer ? ProbeRectJSON(v.layer.bounds) : @{} };
}

@interface ProbeView : NSView
@property(copy) NSString *caption;
@end
@implementation ProbeView
- (void)drawRect:(NSRect)dirtyRect {
    (void)dirtyRect;
    [[NSColor colorWithCalibratedRed:0.08 green:0.18 blue:0.24 alpha:1] setFill];
    NSRectFill(self.bounds);
    [[NSColor colorWithCalibratedRed:0.1 green:0.8 blue:0.65 alpha:1] setFill];
    NSRectFill(NSMakeRect(0, NSHeight(self.bounds) - 5, NSWidth(self.bounds), 5));
    [[NSColor orangeColor] setFill];
    NSRectFill(NSMakeRect(0, 0, NSWidth(self.bounds), 5));
    NSDictionary *attributes = @{ NSFontAttributeName: [NSFont monospacedSystemFontOfSize:15 weight:NSFontWeightRegular],
                                  NSForegroundColorAttributeName: NSColor.whiteColor };
    [(self.caption ?: @"Native AppKit geometry probe") drawAtPoint:NSMakePoint(16, 40) withAttributes:attributes];
}
@end

@interface Probe : NSObject <NSApplicationDelegate, NSWindowDelegate>
@property(strong) NSWindow *window;
@property(strong) ProbeView *child;
@property(strong) NSScreen *targetScreen;
@property(strong) NSArray<NSDictionary *> *cases;
@property(strong) NSMutableArray<NSDictionary *> *outcomes;
@property(strong) NSMutableDictionary *current;
@property(copy) NSString *output;
@property(copy) NSString *state;
@property NSTimeInterval startTime;
@property NSUInteger index;
@property NSUInteger generation;
@property NSUInteger sequence;
@property NSUInteger exceptionCount;
@property BOOL applied;
@property BOOL entered;
@property BOOL exited;
@property BOOL finishing;
@property BOOL summaryWritten;
@property FILE *events;
- (void)record:(NSString *)event extra:(NSDictionary *)extra;
- (void)finish:(NSString *)reason;
@end

@implementation Probe
- (void)record:(NSString *)event extra:(NSDictionary *)extra {
    NSMutableDictionary *row = [NSMutableDictionary dictionaryWithDictionary:extra ?: @{}];
    row[@"event"] = event;
    row[@"sequence"] = @(++self.sequence);
    row[@"elapsedSeconds"] = @([NSDate timeIntervalSinceReferenceDate] - self.startTime);
    row[@"state"] = self.state ?: @"starting";
    if (self.current) row[@"case"] = self.current[@"spec"];
    NSWindow *w = self.window;
    if (w) {
        NSRect frame = w.frame;
        NSRect supplied = NSMakeRect(17, 29, 640, 480);
        row[@"window"] = @{
            @"frame": ProbeRectJSON(frame), @"styleMask": @((NSUInteger)w.styleMask),
            @"fullscreen": @((w.styleMask & NSWindowStyleMaskFullScreen) != 0),
            @"zoomed": @(w.zoomed), @"visible": @(w.visible),
            @"backingScaleFactor": @(w.backingScaleFactor),
            @"contentLayoutRect": ProbeRectJSON(w.contentLayoutRect),
            @"contentRectForActualFrame": ProbeRectJSON([w contentRectForFrameRect:frame]),
            @"instanceContentForSample": ProbeRectJSON([w contentRectForFrameRect:supplied]),
            @"instanceFrameForSample": ProbeRectJSON([w frameRectForContentRect:supplied]),
            @"classContentForSample": ProbeRectJSON([NSWindow contentRectForFrameRect:supplied styleMask:w.styleMask]),
            @"classFrameForSample": ProbeRectJSON([NSWindow frameRectForContentRect:supplied styleMask:w.styleMask]),
            @"contentView": ViewState(w.contentView), @"autoresizedSubview": ViewState(self.child)
        };
        if (w.screen) {
            row[@"screen"] = @{ @"frame": ProbeRectJSON(w.screen.frame),
                                @"visibleFrame": ProbeRectJSON(w.screen.visibleFrame),
                                @"scale": @(w.screen.backingScaleFactor) };
        }
    }
    NSError *error = nil;
    NSData *data = [NSJSONSerialization dataWithJSONObject:row options:NSJSONWritingSortedKeys error:&error];
    if (!data || fwrite(data.bytes, 1, data.length, self.events) != data.length || fputc('\n', self.events) == EOF || fflush(self.events) != 0) {
        fprintf(stderr, "Cannot write event log: %s\n", error.localizedDescription.UTF8String ?: "filesystem error");
        exit(2);
    }
}

- (void)after:(NSTimeInterval)delay run:(void (^)(void))block {
    NSUInteger generation = self.generation;
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)(delay * NSEC_PER_SEC)), dispatch_get_main_queue(), ^{
        if (!self.finishing && generation == self.generation) block();
    });
}

- (void)samples:(NSString *)label {
    [self record:[label stringByAppendingString:@".immediate"] extra:nil];
    [self after:0.1 run:^{ [self record:[label stringByAppendingString:@".after100ms"] extra:nil]; }];
    [self after:0.5 run:^{ [self record:[label stringByAppendingString:@".after500ms"] extra:nil]; }];
}

- (void)applyOperation {
    if (self.applied) return;
    self.applied = YES;
    NSDictionary *spec = self.current[@"spec"];
    NSString *operation = spec[@"operation"];
    NSWindow *w = self.window;
    NSRect before = w.frame;
    NSRect requested = before;
    requested.origin.y += 31;
    requested.size.height = MAX(100, requested.size.height - 31);
    // Store both the intended request and the immediate readback; later samples
    // show whether AppKit ignored, constrained, delayed, or preserved it.
    [self record:@"operation.begin" extra:@{ @"requestedFrame": ProbeRectJSON(requested) }];
    @try {
        if ([operation isEqualToString:@"content-size"]) {
            NSSize size = w.contentView.bounds.size;
            size.height = MAX(100, size.height - 31);
            [self record:@"request.contentSize" extra:@{ @"width": @(size.width), @"height": @(size.height) }];
            [w setContentSize:size];
        } else if ([operation isEqualToString:@"origin"]) {
            NSPoint origin = NSMakePoint(before.origin.x + 23, before.origin.y + 31);
            [self record:@"request.origin" extra:@{ @"x": @(origin.x), @"y": @(origin.y) }];
            [w setFrameOrigin:origin];
        } else if ([operation isEqualToString:@"burst"]) {
            CGFloat heights[] = {NSHeight(before) - 33, NSHeight(before) - 31,
                                 NSHeight(before) * 0.5, NSHeight(before) + 71};
            for (NSUInteger i = 0; i < 4; i++) {
                NSRect r = before;
                r.origin.y += 31;
                r.size.height = MAX(100, heights[i]);
                [self record:@"request.burst" extra:@{ @"iteration": @(i), @"requestedFrame": ProbeRectJSON(r) }];
                [w setFrame:r display:NO];
                [self record:@"request.burst.returned" extra:@{ @"iteration": @(i) }];
            }
        } else if ([operation isEqualToString:@"roblox-transition"]) {
            NSWindowStyleMask mask = w.styleMask & ~NSWindowStyleMaskFullSizeContentView;
            [self record:@"request.styleMask" extra:@{ @"requestedStyleMask": @((NSUInteger)mask) }];
            w.styleMask = mask;
            [self record:@"request.styleMask.returned" extra:nil];
            NSRect screen = w.screen ? w.screen.frame : before;
            for (NSUInteger i = 0; i < 2; i++) {
                NSRect r = screen;
                r.origin.y += (i == 0 ? 32 : 31);
                r.size.height = MAX(100, NSHeight(screen) - (i == 0 ? 33 : 31));
                [self record:@"request.transitionFrame" extra:@{ @"iteration": @(i), @"requestedFrame": ProbeRectJSON(r) }];
                [w setFrame:r display:NO];
                [self record:@"request.transitionFrame.returned" extra:@{ @"iteration": @(i) }];
            }
        } else if ([operation isEqualToString:@"frame-display"]) {
            [w setFrame:requested display:YES];
        } else if ([operation isEqualToString:@"frame-animate"]) {
            [w setFrame:requested display:YES animate:YES];
        } else {
            [w setFrame:requested display:NO];
        }
    } @catch (NSException *exception) {
        self.exceptionCount++;
        self.current[@"exceptions"] = @([self.current[@"exceptions"] unsignedIntegerValue] + 1);
        [self record:@"operation.exception" extra:@{ @"name": exception.name, @"reason": exception.reason ?: @"" }];
    }
    [self samples:@"operation.returned"];
}

- (void)enterFullscreen {
    self.state = @"requesting-enter";
    self.current[@"preFullscreenFrame"] = ProbeRectJSON(self.window.frame);
    self.current[@"preFullscreenContentView"] = ViewState(self.window.contentView);
    [self record:@"toggle.enter.before" extra:nil];
    [self.window toggleFullScreen:nil];
    [self record:@"toggle.enter.returned" extra:nil];
}

- (void)beginCase {
    if (self.index >= self.cases.count) { [self finish:@"completed"]; return; }
    self.generation++;
    self.applied = self.entered = self.exited = NO;
    NSDictionary *spec = self.cases[self.index];
    self.current = [@{ @"spec": spec, @"exceptions": @0 } mutableCopy];
    self.state = @"windowed";
    NSWindowStyleMask style = NSWindowStyleMaskTitled | NSWindowStyleMaskClosable |
                              NSWindowStyleMaskMiniaturizable | NSWindowStyleMaskResizable;
    BOOL fullSize = [spec[@"style"] isEqualToString:@"full-size-content"];
    if (fullSize) style |= NSWindowStyleMaskFullSizeContentView;
    self.window = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 720, 460)
                                           styleMask:style backing:NSBackingStoreBuffered defer:NO screen:self.targetScreen];
    self.window.releasedWhenClosed = NO;
    self.window.delegate = self;
    self.window.collectionBehavior = NSWindowCollectionBehaviorFullScreenPrimary;
    self.window.titlebarAppearsTransparent = fullSize;
    self.window.title = [NSString stringWithFormat:@"Rosemary probe %lu/%lu: %@ / %@ / %@",
                         (unsigned long)self.index + 1, (unsigned long)self.cases.count,
                         spec[@"style"], spec[@"operation"], spec[@"phase"]];
    NSView *root = [[NSView alloc] initWithFrame:NSMakeRect(0, 0, 720, 460)];
    root.wantsLayer = YES;
    self.window.contentView = root;
    self.child = [[ProbeView alloc] initWithFrame:root.bounds];
    self.child.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
    self.child.wantsLayer = YES;
    self.child.caption = [NSString stringWithFormat:@"%@\n\nAutomatic native AppKit measurement.\nPlease leave windows alone. Cmd-Q stops and saves.", self.window.title];
    [root addSubview:self.child];
    NSRect visible = self.targetScreen.visibleFrame;
    [self.window setFrameOrigin:NSMakePoint(NSMidX(visible) - NSWidth(self.window.frame) / 2,
                                           NSMidY(visible) - NSHeight(self.window.frame) / 2)];
    [self.window makeKeyAndOrderFront:nil];
    [NSApp activateIgnoringOtherApps:YES];
    [self record:@"case.begin" extra:nil];
    printf("[%lu/%lu] %s / %s / %s\n", (unsigned long)self.index + 1,
           (unsigned long)self.cases.count, [spec[@"style"] UTF8String],
           [spec[@"operation"] UTF8String], [spec[@"phase"] UTF8String]);
    fflush(stdout);
    [self after:35 run:^{ [self record:@"case.timeout" extra:nil]; [self finish:@"transition-timeout"]; }];
    [self after:0.6 run:^{
        [self record:@"windowed.baseline" extra:nil];
        self.current[@"initialFrame"] = ProbeRectJSON(self.window.frame);
        NSString *phase = spec[@"phase"];
        if ([phase isEqualToString:@"zoomed"]) {
            [self.window zoom:nil];
            [self after:0.8 run:^{
                self.state = @"zoomed";
                [self record:@"zoomed.beforeOperation" extra:nil];
                [self applyOperation];
                [self after:0.9 run:^{
                    [self record:@"zoomed.afterOperation" extra:nil];
                    if (self.window.zoomed) [self.window zoom:nil];
                    [self after:0.8 run:^{
                        [self record:@"zoomed.restored" extra:nil];
                        [self enterFullscreen];
                    }];
                }];
            }];
        } else if ([phase isEqualToString:@"windowed"]) {
            [self applyOperation];
            [self after:0.9 run:^{ [self enterFullscreen]; }];
        } else {
            [self enterFullscreen];
        }
    }];
}

- (void)windowWillEnterFullScreen:(NSNotification *)notification {
    (void)notification;
    self.state = @"will-enter";
    [self record:@"notification.willEnterFullscreen" extra:nil];
    if ([self.current[@"spec"][@"phase"] isEqualToString:@"will-enter"]) [self applyOperation];
}

- (void)windowDidEnterFullScreen:(NSNotification *)notification {
    (void)notification;
    self.state = @"fullscreen";
    [self record:@"notification.didEnterFullscreen" extra:nil];
    if (self.entered) return;
    self.entered = YES;
    [self after:0.8 run:^{
        [self record:@"fullscreen.settled" extra:nil];
        if ([self.current[@"spec"][@"phase"] isEqualToString:@"fullscreen"]) [self applyOperation];
        [self after:0.9 run:^{
            self.state = @"requesting-exit";
            [self record:@"toggle.exit.before" extra:nil];
            [self.window toggleFullScreen:nil];
            [self record:@"toggle.exit.returned" extra:nil];
        }];
    }];
}

- (void)windowWillExitFullScreen:(NSNotification *)notification {
    (void)notification;
    self.state = @"will-exit";
    [self record:@"notification.willExitFullscreen" extra:nil];
    if ([self.current[@"spec"][@"phase"] isEqualToString:@"will-exit"]) [self applyOperation];
}

- (void)windowDidExitFullScreen:(NSNotification *)notification {
    (void)notification;
    self.state = @"restored";
    [self record:@"notification.didExitFullscreen" extra:nil];
    if (self.exited) return;
    self.exited = YES;
    [self after:0.9 run:^{
        [self record:@"restored.settled" extra:nil];
        self.current[@"restoredFrame"] = ProbeRectJSON(self.window.frame);
        self.current[@"restoredContentView"] = ViewState(self.window.contentView);
        // Ensure a fullscreen request has not broken later ordinary resizing.
        NSSize size = NSMakeSize(640, 420);
        [self record:@"postExitResize.before" extra:@{ @"requestedContentWidth": @640, @"requestedContentHeight": @420 }];
        [self.window setContentSize:size];
        [self samples:@"postExitResize"];
        [self after:0.8 run:^{
            [self record:@"case.end" extra:@{ @"operationApplied": @(self.applied) }];
            self.current[@"operationApplied"] = @(self.applied);
            self.current[@"finalFrame"] = ProbeRectJSON(self.window.frame);
            [self.outcomes addObject:[self.current copy]];
            self.window.delegate = nil;
            [self.window close];
            self.window = nil;
            self.child = nil;
            self.current = nil;
            self.index++;
            self.generation++;
            [self after:0.25 run:^{ [self beginCase]; }];
        }];
    }];
}

- (void)windowDidFailToEnterFullScreen:(NSWindow *)window {
    (void)window;
    [self record:@"notification.failedToEnterFullscreen" extra:nil];
    [self finish:@"fullscreen-entry-failed"];
}
- (void)windowDidFailToExitFullScreen:(NSWindow *)window {
    (void)window;
    [self record:@"notification.failedToExitFullscreen" extra:nil];
    [self finish:@"fullscreen-exit-failed"];
}
- (void)windowDidResize:(NSNotification *)notification {
    (void)notification;
    [self record:@"notification.didResize" extra:nil];
}
- (void)windowDidMove:(NSNotification *)notification {
    (void)notification;
    [self record:@"notification.didMove" extra:nil];
}
- (void)windowDidChangeBackingProperties:(NSNotification *)notification {
    (void)notification;
    [self record:@"notification.didChangeBackingProperties" extra:nil];
}
- (BOOL)windowShouldClose:(NSWindow *)sender {
    (void)sender;
    [self finish:@"stopped-by-user"];
    return NO;
}

- (void)writeSummary:(NSString *)reason {
    if (self.summaryWritten) return;
    self.summaryWritten = YES;
    [self record:@"run.end" extra:@{ @"reason": reason }];
    NSMutableDictionary *summary = [@{
        @"schemaVersion": @1, @"reason": reason, @"plannedCases": @(self.cases.count),
        @"completedCases": @(self.outcomes.count), @"exceptionCount": @(self.exceptionCount),
        @"semanticVerdict": @"observational; native behavior is the oracle, not a preselected expectation",
        @"outcomes": self.outcomes
    } mutableCopy];
    if (self.current) summary[@"interruptedCase"] = self.current;
    NSError *error = nil;
    NSData *data = [NSJSONSerialization dataWithJSONObject:summary options:NSJSONWritingPrettyPrinted | NSJSONWritingSortedKeys error:&error];
    NSString *path = [self.output stringByAppendingPathComponent:@"summary.json"];
    if (!data || ![data writeToFile:path options:NSDataWritingAtomic error:&error]) {
        fprintf(stderr, "Cannot write summary: %s\n", error.localizedDescription.UTF8String);
    }
    printf("\nRun ended: %s. %lu/%lu cases completed; %lu caught exceptions.\nResults: %s\n",
           reason.UTF8String, (unsigned long)self.outcomes.count, (unsigned long)self.cases.count,
           (unsigned long)self.exceptionCount, self.output.UTF8String);
    fflush(stdout);
}
- (void)finish:(NSString *)reason {
    if (self.finishing) return;
    self.finishing = YES;
    [self writeSummary:reason];
    [NSApp terminate:nil];
}
- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)sender {
    (void)sender;
    self.finishing = YES;
    [self writeSummary:@"stopped-by-user"];
    return NSTerminateNow;
}
- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    (void)notification;
    NSMenu *menu = [[NSMenu alloc] initWithTitle:@"Main"];
    NSMenuItem *appItem = [[NSMenuItem alloc] initWithTitle:@"Probe" action:NULL keyEquivalent:@""];
    [menu addItem:appItem];
    NSMenu *appMenu = [[NSMenu alloc] initWithTitle:@"Probe"];
    [appMenu addItemWithTitle:@"Stop and save results" action:@selector(terminate:) keyEquivalent:@"q"];
    appItem.submenu = appMenu;
    NSApp.mainMenu = menu;
    [self beginCase];
}
@end

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        NSString *output = nil;
        BOOL quick = NO, list = NO;
        NSInteger screenIndex = 0;
        NSString *phaseFilter = nil, *operationFilter = nil, *styleFilter = nil;
        for (int i = 1; i < argc; i++) {
            NSString *arg = [NSString stringWithUTF8String:argv[i]];
            if ([arg isEqualToString:@"--quick"]) quick = YES;
            else if ([arg isEqualToString:@"--list"]) list = YES;
            else if (([arg isEqualToString:@"--output"] || [arg isEqualToString:@"--phase"] ||
                      [arg isEqualToString:@"--operation"] || [arg isEqualToString:@"--style"] ||
                      [arg isEqualToString:@"--screen"]) && i + 1 < argc) {
                NSString *value = [NSString stringWithUTF8String:argv[++i]];
                if ([arg isEqualToString:@"--output"]) output = value;
                else if ([arg isEqualToString:@"--phase"]) phaseFilter = value;
                else if ([arg isEqualToString:@"--operation"]) operationFilter = value;
                else if ([arg isEqualToString:@"--style"]) styleFilter = value;
                else {
                    if (!value.length || [value rangeOfCharacterFromSet:NSCharacterSet.decimalDigitCharacterSet.invertedSet].location != NSNotFound) {
                        fprintf(stderr, "--screen requires a nonnegative display index.\n"); return 2;
                    }
                    screenIndex = value.integerValue;
                }
            } else {
                fprintf(stderr, "Usage: WindowProbe --output DIR [--quick] [--list] [--phase PHASE] [--operation OP] [--style STYLE] [--screen INDEX]\n");
                return 2;
            }
        }
        NSArray *styles = quick ? @[@"standard"] : @[@"standard", @"full-size-content"];
        NSArray *operations = quick ? @[@"frame", @"content-size"] :
            @[@"frame", @"frame-display", @"frame-animate", @"content-size", @"origin", @"burst", @"roblox-transition"];
        NSArray *phases = @[@"windowed", @"zoomed", @"will-enter", @"fullscreen", @"will-exit"];
        NSMutableArray *cases = [NSMutableArray array];
        for (NSString *style in styles) for (NSString *operation in operations) for (NSString *phase in phases) {
            if ((styleFilter && ![styleFilter isEqual:style]) ||
                (operationFilter && ![operationFilter isEqual:operation]) ||
                (phaseFilter && ![phaseFilter isEqual:phase])) continue;
            [cases addObject:@{ @"id": @(cases.count + 1), @"style": style, @"operation": operation, @"phase": phase }];
        }
        if (!cases.count) { fprintf(stderr, "No cases match the supplied filters.\n"); return 2; }
        if (list) {
            NSData *data = [NSJSONSerialization dataWithJSONObject:cases options:NSJSONWritingPrettyPrinted error:nil];
            fwrite(data.bytes, 1, data.length, stdout); putchar('\n'); return 0;
        }
        if (!output) { fprintf(stderr, "--output DIR is required.\n"); return 2; }
        NSError *error = nil;
        if (![[NSFileManager defaultManager] createDirectoryAtPath:output withIntermediateDirectories:YES attributes:nil error:&error]) {
            fprintf(stderr, "Cannot create results folder: %s\n", error.localizedDescription.UTF8String); return 2;
        }
        Probe *probe = [[Probe alloc] init];
        probe.output = output;
        probe.cases = cases;
        probe.outcomes = [NSMutableArray array];
        probe.startTime = [NSDate timeIntervalSinceReferenceDate];
        probe.events = fopen([[output stringByAppendingPathComponent:@"events.jsonl"] fileSystemRepresentation], "wx");
        if (!probe.events) { perror("Create new events.jsonl"); return 2; }
        struct utsname host; uname(&host);
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
        NSApp.delegate = probe;
        if (screenIndex < 0 || (NSUInteger)screenIndex >= NSScreen.screens.count) {
            fprintf(stderr, "Display index is out of range: %ld; connected displays: %lu.\n",
                    (long)screenIndex, (unsigned long)NSScreen.screens.count);
            fclose(probe.events); return 2;
        }
        probe.targetScreen = NSScreen.screens[(NSUInteger)screenIndex];
        NSMutableArray *screens = [NSMutableArray array];
        for (NSScreen *screen in NSScreen.screens) {
            NSEdgeInsets insets = screen.safeAreaInsets;
            [screens addObject:@{ @"frame": ProbeRectJSON(screen.frame), @"visibleFrame": ProbeRectJSON(screen.visibleFrame),
                                 @"scale": @(screen.backingScaleFactor),
                                 @"safeAreaInsets": @{ @"top": @(insets.top), @"bottom": @(insets.bottom),
                                                       @"left": @(insets.left), @"right": @(insets.right) } }];
        }
        [probe record:@"run.begin" extra:@{ @"schemaVersion": @1,
            @"osVersion": NSProcessInfo.processInfo.operatingSystemVersionString,
            @"processArchitecture": @"arm64", @"hostArchitecture": [NSString stringWithUTF8String:host.machine],
            @"screens": screens, @"selectedScreenIndex": @(screenIndex), @"cases": cases,
            @"timeUTC": [[NSISO8601DateFormatter new] stringFromDate:NSDate.date] }];
        [NSApp run];
        fclose(probe.events);
    }
    return 0;
}

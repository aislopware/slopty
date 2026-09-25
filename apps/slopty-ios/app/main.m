// The UIKit shell for Slopty on iOS. Everything else is Rust (`slopty_ios_run` in
// apps/slopty-ios); this file only exists because UIApplicationMain and the scene delegate
// must be Objective-C classes. The gpui_ios_* symbols come from the gpui_ios crate.
#import <UIKit/UIKit.h>

extern void slopty_ios_run(void);
extern void gpui_ios_will_enter_foreground(void *application);
extern void gpui_ios_did_become_active(void *application);
extern void gpui_ios_will_resign_active(void *application);
extern void gpui_ios_did_enter_background(void *application);
extern void gpui_ios_did_receive_memory_warning(void *application);
extern void gpui_ios_will_terminate(void *application);
extern void gpui_ios_handle_open_url(void *url);
extern void gpui_ios_set_window_scene(void *scene);

@interface SloptySceneDelegate : UIResponder <UIWindowSceneDelegate>
@end

@interface SloptyAppDelegate : UIResponder <UIApplicationDelegate>
@end

@implementation SloptySceneDelegate

- (void)scene:(UIScene *)scene
    willConnectToSession:(UISceneSession *)session
                 options:(UISceneConnectionOptions *)connectionOptions {
    if (![scene isKindOfClass:UIWindowScene.class]) {
        return;
    }
    gpui_ios_set_window_scene((__bridge void *)scene);
    // Each window drives its own display link, paused while idle (gpui_ios frame pacing).
    slopty_ios_run();
}

- (void)sceneWillEnterForeground:(UIScene *)scene {
    gpui_ios_will_enter_foreground((__bridge void *)scene);
}

- (void)sceneDidBecomeActive:(UIScene *)scene {
    gpui_ios_did_become_active((__bridge void *)scene);
}

- (void)sceneWillResignActive:(UIScene *)scene {
    gpui_ios_will_resign_active((__bridge void *)scene);
}

- (void)sceneDidEnterBackground:(UIScene *)scene {
    gpui_ios_did_enter_background((__bridge void *)scene);
}

- (void)scene:(UIScene *)scene openURLContexts:(NSSet<UIOpenURLContext *> *)URLContexts {
    UIOpenURLContext *context = URLContexts.anyObject;
    if (context != nil) {
        gpui_ios_handle_open_url((__bridge void *)context.URL.absoluteString);
    }
}

@end

@implementation SloptyAppDelegate

- (BOOL)application:(UIApplication *)application
    didFinishLaunchingWithOptions:(NSDictionary *)launchOptions {
    return YES;
}

- (UISceneConfiguration *)application:(UIApplication *)application
    configurationForConnectingSceneSession:(UISceneSession *)connectingSceneSession
                                   options:(UISceneConnectionOptions *)options {
    UISceneConfiguration *configuration =
        [[UISceneConfiguration alloc] initWithName:@"Default Configuration"
                                       sessionRole:connectingSceneSession.role];
    configuration.delegateClass = SloptySceneDelegate.class;
    return configuration;
}

- (void)applicationDidReceiveMemoryWarning:(UIApplication *)application {
    gpui_ios_did_receive_memory_warning((__bridge void *)application);
}

- (void)applicationWillTerminate:(UIApplication *)application {
    gpui_ios_will_terminate((__bridge void *)application);
}

@end

int main(int argc, char *argv[]) {
    @autoreleasepool {
        return UIApplicationMain(argc, argv, nil, NSStringFromClass(SloptyAppDelegate.class));
    }
}

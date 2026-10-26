#import "notifications.h"
#import "../app.h"

#import <UserNotifications/UserNotifications.h>

void requestNotificationPermissionsWithCompletionHandler(
    void (^completion_handler)(NSUInteger outcome_type, id outcome_msg)) {
    UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];

    [center
        requestAuthorizationWithOptions:(UNAuthorizationOptionAlert + UNAuthorizationOptionSound +
                                         UNAuthorizationOptionBadge)
                      completionHandler:^(BOOL granted, NSError *_Nullable error) {
                        if (!granted) {
                            completion_handler(1, @"User denied request to receive notifications.");
                        } else if (error != nil) {
                            completion_handler(2, error.localizedDescription);
                        } else {
                            registerNotificationCategories(center);
                            completion_handler(0,
                                               @"User accepted request to receive notifications.");
                        }
                      }];
}

void registerNotificationCategories(id center_) {
    UNUserNotificationCenter *center = center_;
    UNNotificationCategory *customized =
        [UNNotificationCategory categoryWithIdentifier:@"CUSTOMIZED_NOTIFICATION"
                                               actions:@[]
                                     intentIdentifiers:@[]
                                               options:UNNotificationCategoryOptionCustomDismissAction];
    // Harp: a Claude permission dialog (Allow, Deny, Open chat) and any other Claude dialog
    // (Open chat). Allow and Deny answer without bringing Harp forward.
    UNNotificationAction *allow =
        [UNNotificationAction actionWithIdentifier:@"approve-0"
                                             title:@"Allow"
                                           options:UNNotificationActionOptionNone];
    UNNotificationAction *deny =
        [UNNotificationAction actionWithIdentifier:@"approve-deny"
                                             title:@"Deny"
                                           options:UNNotificationActionOptionDestructive];
    UNNotificationAction *open =
        [UNNotificationAction actionWithIdentifier:@"open"
                                             title:@"Open chat"
                                           options:UNNotificationActionOptionForeground];
    UNNotificationCategory *approval =
        [UNNotificationCategory categoryWithIdentifier:@"HARP_APPROVAL"
                                               actions:@[ allow, deny, open ]
                                     intentIdentifiers:@[]
                                               options:UNNotificationCategoryOptionNone];
    UNNotificationCategory *question =
        [UNNotificationCategory categoryWithIdentifier:@"HARP_QUESTION"
                                               actions:@[ open ]
                                     intentIdentifiers:@[]
                                               options:UNNotificationCategoryOptionNone];
    [center setNotificationCategories:[NSSet setWithObjects:customized, approval, question, nil]];
}

void removeNotification(id identifier) {
    // Unbundled (`cargo run`) there is no notification center to ask.
    NSString *bundleIdentifier = [[NSBundle mainBundle] bundleIdentifier];
    if (bundleIdentifier == nil || [bundleIdentifier length] == 0) return;
    UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
    NSArray *ids = @[ identifier ];
    [center removePendingNotificationRequestsWithIdentifiers:ids];
    [center removeDeliveredNotificationsWithIdentifiers:ids];
}

void requestNotificationPermissions(void *on_completion_callback) {
    requestNotificationPermissionsWithCompletionHandler(^(NSUInteger outcome_type, id outcome_msg) {
      dispatch_async(dispatch_get_main_queue(), ^{
        warp_on_request_notification_permissions_completed(outcome_type, outcome_msg,
                                                           on_completion_callback);
      });
    });
}

void sendNotificationWithErrorHandler(NSString *title, NSString *subtitle, NSString *body,
                                      NSString *data, NSString *identifier, NSString *category,
                                      void (^error_handler)(NSUInteger error_type, id error_msg),
                                      BOOL playSound) {
    UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
    [center getNotificationSettingsWithCompletionHandler:^(UNNotificationSettings *settings) {
      if (settings.authorizationStatus == UNAuthorizationStatusDenied) {
          error_handler(0, @"User turned permissions off in system preferences.");
      } else {
          // Create the notification content.
          // `autorelease` balances the +1 retain from `alloc`; the enclosing UserNotifications
          // completion block runs on a GCD-dispatched queue that drains an ambient pool.
          UNMutableNotificationContent *content =
              [[[UNMutableNotificationContent alloc] init] autorelease];
          content.title = [NSString localizedUserNotificationStringForKey:title arguments:nil];
          content.body = [NSString localizedUserNotificationStringForKey:body arguments:nil];
          if ([subtitle length] > 0) {
              content.subtitle = subtitle;
          }
          if ([category length] > 0) {
              content.categoryIdentifier = category;
          }

          // Only play sound if the user setting allows it
          if (playSound) {
              content.sound = [UNNotificationSound defaultSound];
          }

          content.userInfo = @{
              @"DATA" : data,
          };

          // Configure the trigger to send the notification after 1 second.
          UNTimeIntervalNotificationTrigger *trigger =
              [UNTimeIntervalNotificationTrigger triggerWithTimeInterval:1 repeats:NO];

          // Create the request object.
          UNNotificationRequest *request =
              [UNNotificationRequest requestWithIdentifier:([identifier length] > 0
                                                                ? identifier
                                                                : @"CUSTOMIZED_NOTIFICATION")
                                                   content:content
                                                   trigger:trigger];

          // Schedule the notification.
          [center addNotificationRequest:request
                   withCompletionHandler:^(NSError *_Nullable err) {
                     if (err != nil) {
                         error_handler(1, err.localizedDescription);
                     }
                   }];
      }
    }];
}

void sendNotification(id title, id subtitle, id body, id data, id identifier, id category,
                      void *on_error_callback, BOOL playSound) {
    sendNotificationWithErrorHandler(
        title, subtitle, body, data, identifier, category,
        ^(NSUInteger error_type, id error_msg) {
          dispatch_async(dispatch_get_main_queue(), ^{
            warp_on_notification_send_error(error_type, error_msg, on_error_callback);
          });
        },
        playSound);
}

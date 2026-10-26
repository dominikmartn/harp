#import <AppKit/AppKit.h>

// Requests authorization for notifications.
void requestNotificationPermissions(void* callback);

// This method, implemented in Rust, invokes the callback to allow the App to
// take action when the user has responded to the permissions request.
void warp_on_request_notification_permissions_completed(NSUInteger outcome_type, id outcome_msg,
                                                        void* callback);

// Registers every notification category: Warp's own and Harp's Claude ones (with actions).
// `setNotificationCategories` replaces the whole set, so every caller goes through this.
void registerNotificationCategories(id center);

// Sends a desktop notification: title, subtitle, body, data, identifier (empty = Warp's one
// fixed id, so a newer one replaces it), category (empty = none), error callback, sound.
void sendNotification(id, id, id, id, id, id, void*, BOOL);

// Takes back a pending or delivered notification by its identifier.
void removeNotification(id identifier);

// This method, implemented in Rust, invokes the callback to allow the App to
// take action when a notification fails to send.
void warp_on_notification_send_error(NSUInteger error_type, id error_msg, void* callback);

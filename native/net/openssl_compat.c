/*
 * The stock VitaSDK libcurl archive was built against the old OpenSSL
 * package, while the SDK currently installed in the build image provides
 * OpenSSL 1.1.1. YUNYIN builds its own libcurl against the current SDK and
 * needs only this one legacy UI symbol, which is not exported by the Vita
 * OpenSSL build.
 *
 * Returning OpenSSL's non-interactive UI method is safe for this client:
 * YUNYIN never loads a private key or asks for a password.
 */

#include <openssl/ui.h>

UI_METHOD *UI_OpenSSL(void) {
    return (UI_METHOD *)UI_null();
}

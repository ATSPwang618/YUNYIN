/*
 * The stock VitaSDK libcurl archive was built against an old OpenSSL package,
 * while YUNYIN links its own libcurl against whatever OpenSSL the build image
 * ships.  That libcurl references UI_OpenSSL().
 *
 * OpenSSL 1.1.0 removed UI_OpenSSL() and replaced it with UI_null() (the
 * non-interactive method).  So:
 *   * image ships OpenSSL 1.1.x -> libcrypto no longer exports UI_OpenSSL,
 *     and this shim has to provide it in terms of UI_null();
 *   * image ships OpenSSL 1.0.2 (VitaSDK 2026.08 here: 0x10002090) -> libcrypto
 *     already exports UI_OpenSSL and has no UI_null(), so defining it here would
 *     both fail to compile and clash at link time.
 *
 * Returning the non-interactive UI method is safe for this client either way:
 * YUNYIN never loads a private key or asks for a password.
 */

#include <openssl/opensslv.h>
#include <openssl/ui.h>

#if OPENSSL_VERSION_NUMBER >= 0x10100000L
UI_METHOD *UI_OpenSSL(void) {
    return (UI_METHOD *)UI_null();
}
#endif

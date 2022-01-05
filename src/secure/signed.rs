use std::convert::TryInto;
use std::borrow::{Borrow, BorrowMut};

use base64ct::{Base64, Encoding};
use sha2::{Sha256};
use hmac::{Hmac, Mac, digest::Output};

use crate::secure::Key;
use crate::{Cookie, CookieJar};

// Keep these in sync, and keep the key len synced with the `signed` docs as
// well as the `KEYS_INFO` const in secure::Key.
pub(crate) const BASE64_DIGEST_LEN: usize = 44;
pub(crate) const KEY_LEN: usize = 32;

/// A child cookie jar that authenticates its cookies.
///
/// A _signed_ child jar signs all the cookies added to it and verifies cookies
/// retrieved from it. Any cookies stored in a `SignedJar` are provided
/// integrity and authenticity. In other words, clients cannot tamper with the
/// contents of a cookie nor can they fabricate cookie values, but the data is
/// visible in plaintext.
#[cfg_attr(all(nightly, doc), doc(cfg(feature = "signed")))]
pub struct SignedJar<J> {
    parent: J,
    key: [u8; KEY_LEN],
}

impl<J> SignedJar<J> {
    /// Creates a new child `SignedJar` with parent `parent` and key `key`. This
    /// method is typically called indirectly via the `signed{_mut}` methods of
    /// `CookieJar`.
    pub(crate) fn new(parent: J, key: &Key) -> SignedJar<J> {
        SignedJar { parent, key: key.signing().try_into().expect("sign key len") }
    }

    /// Signs the cookie's value providing integrity and authenticity.
    fn sign_cookie(&self, cookie: &mut Cookie) {
        // Compute HMAC-SHA256 of the cookie's value.
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("good key");
        mac.update(cookie.value().as_bytes());

        // Cookie's new value is [MAC | original-value].
        let tag = mac.finalize().into_bytes();
        let mut new_value = Base64::encode_string(&tag);
        new_value.push_str(cookie.value());
        cookie.set_value(new_value);
    }

    /// Given a signed value `str` where the signature is prepended to `value`,
    /// verifies the signed value and returns it. If there's a problem, returns
    /// an `Err` with a string describing the issue.
    fn _verify(&self, cookie_value: &str) -> Result<String, &'static str> {
        if !cookie_value.is_char_boundary(BASE64_DIGEST_LEN) {
            return Err("missing or invalid digest");
        }

        // Split [MAC | original-value] into its two parts.
        let (digest_str, value) = cookie_value.split_at(BASE64_DIGEST_LEN);
        let mut digest: Output<Hmac<Sha256>> = Default::default();
        Base64::decode(digest_str, &mut digest).map_err(|_| "bad base64 digest")?;

        // Perform the verification.
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("good key");
        mac.update(value.as_bytes());
        mac.verify(&digest)
            .map(|_| value.to_string())
            .map_err(|_| "value did not verify")
    }

    /// Verifies the authenticity and integrity of `cookie`, returning the
    /// plaintext version if verification succeeds or `None` otherwise.
    /// Verification _always_ succeeds if `cookie` was generated by a
    /// `SignedJar` with the same key as `self`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cookie::{CookieJar, Cookie, Key};
    ///
    /// let key = Key::generate();
    /// let mut jar = CookieJar::new();
    /// assert!(jar.signed(&key).get("name").is_none());
    ///
    /// jar.signed_mut(&key).add(Cookie::new("name", "value"));
    /// assert_eq!(jar.signed(&key).get("name").unwrap().value(), "value");
    ///
    /// let plain = jar.get("name").cloned().unwrap();
    /// assert_ne!(plain.value(), "value");
    /// let verified = jar.signed(&key).verify(plain).unwrap();
    /// assert_eq!(verified.value(), "value");
    ///
    /// let plain = Cookie::new("plaintext", "hello");
    /// assert!(jar.signed(&key).verify(plain).is_none());
    /// ```
    pub fn verify(&self, mut cookie: Cookie<'static>) -> Option<Cookie<'static>> {
        if let Ok(value) = self._verify(cookie.value()) {
            cookie.set_value(value);
            return Some(cookie);
        }

        None
    }
}

impl<J: Borrow<CookieJar>> SignedJar<J> {
    /// Returns a reference to the `Cookie` inside this jar with the name `name`
    /// and verifies the authenticity and integrity of the cookie's value,
    /// returning a `Cookie` with the authenticated value. If the cookie cannot
    /// be found, or the cookie fails to verify, `None` is returned.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cookie::{CookieJar, Cookie, Key};
    ///
    /// let key = Key::generate();
    /// let jar = CookieJar::new();
    /// assert!(jar.signed(&key).get("name").is_none());
    ///
    /// let mut jar = jar;
    /// let mut signed_jar = jar.signed_mut(&key);
    /// signed_jar.add(Cookie::new("name", "value"));
    /// assert_eq!(signed_jar.get("name").unwrap().value(), "value");
    /// ```
    pub fn get(&self, name: &str) -> Option<Cookie<'static>> {
        self.parent.borrow().get(name).and_then(|c| self.verify(c.clone()))
    }
}

impl<J: BorrowMut<CookieJar>> SignedJar<J> {
    /// Adds `cookie` to the parent jar. The cookie's value is signed assuring
    /// integrity and authenticity.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cookie::{CookieJar, Cookie, Key};
    ///
    /// let key = Key::generate();
    /// let mut jar = CookieJar::new();
    /// jar.signed_mut(&key).add(Cookie::new("name", "value"));
    ///
    /// assert_ne!(jar.get("name").unwrap().value(), "value");
    /// assert!(jar.get("name").unwrap().value().contains("value"));
    /// assert_eq!(jar.signed(&key).get("name").unwrap().value(), "value");
    /// ```
    pub fn add(&mut self, mut cookie: Cookie<'static>) {
        self.sign_cookie(&mut cookie);
        self.parent.borrow_mut().add(cookie);
    }

    /// Adds an "original" `cookie` to this jar. The cookie's value is signed
    /// assuring integrity and authenticity. Adding an original cookie does not
    /// affect the [`CookieJar::delta()`] computation. This method is intended
    /// to be used to seed the cookie jar with cookies received from a client's
    /// HTTP message.
    ///
    /// For accurate `delta` computations, this method should not be called
    /// after calling `remove`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cookie::{CookieJar, Cookie, Key};
    ///
    /// let key = Key::generate();
    /// let mut jar = CookieJar::new();
    /// jar.signed_mut(&key).add_original(Cookie::new("name", "value"));
    ///
    /// assert_eq!(jar.iter().count(), 1);
    /// assert_eq!(jar.delta().count(), 0);
    /// ```
    pub fn add_original(&mut self, mut cookie: Cookie<'static>) {
        self.sign_cookie(&mut cookie);
        self.parent.borrow_mut().add_original(cookie);
    }

    /// Removes `cookie` from the parent jar.
    ///
    /// For correct removal, the passed in `cookie` must contain the same `path`
    /// and `domain` as the cookie that was initially set.
    ///
    /// This is identical to [`CookieJar::remove()`]. See the method's
    /// documentation for more details.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cookie::{CookieJar, Cookie, Key};
    ///
    /// let key = Key::generate();
    /// let mut jar = CookieJar::new();
    /// let mut signed_jar = jar.signed_mut(&key);
    ///
    /// signed_jar.add(Cookie::new("name", "value"));
    /// assert!(signed_jar.get("name").is_some());
    ///
    /// signed_jar.remove(Cookie::named("name"));
    /// assert!(signed_jar.get("name").is_none());
    /// ```
    pub fn remove(&mut self, cookie: Cookie<'static>) {
        self.parent.borrow_mut().remove(cookie);
    }
}

#[cfg(test)]
mod test {
    use crate::{CookieJar, Cookie, Key};

    #[test]
    fn simple() {
        let key = Key::generate();
        let mut jar = CookieJar::new();
        assert_simple_behaviour!(jar, jar.signed_mut(&key));
    }

    #[test]
    fn private() {
        let key = Key::generate();
        let mut jar = CookieJar::new();
        assert_secure_behaviour!(jar, jar.signed_mut(&key));
    }

    #[test]
    fn roundtrip() {
        // Secret is SHA-256 hash of 'Super secret!' passed through HKDF-SHA256.
        let key = Key::from(&[89, 202, 200, 125, 230, 90, 197, 245, 166, 249,
            34, 169, 135, 31, 20, 197, 94, 154, 254, 79, 60, 26, 8, 143, 254,
            24, 116, 138, 92, 225, 159, 60, 157, 41, 135, 129, 31, 226, 196, 16,
            198, 168, 134, 4, 42, 1, 196, 24, 57, 103, 241, 147, 201, 185, 233,
            10, 180, 170, 187, 89, 252, 137, 110, 107]);

        let mut jar = CookieJar::new();
        jar.add(Cookie::new("signed_with_ring014",
                "3tdHXEQ2kf6fxC7dWzBGmpSLMtJenXLKrZ9cHkSsl1w=Tamper-proof"));
        jar.add(Cookie::new("signed_with_ring016",
                "3tdHXEQ2kf6fxC7dWzBGmpSLMtJenXLKrZ9cHkSsl1w=Tamper-proof"));

        let signed = jar.signed(&key);
        assert_eq!(signed.get("signed_with_ring014").unwrap().value(), "Tamper-proof");
        assert_eq!(signed.get("signed_with_ring016").unwrap().value(), "Tamper-proof");
    }

    #[test]
    fn issue_178() {
        let data = "x=yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy£";
        let c = Cookie::parse(data).expect("failed to parse cookie");
        let key = Key::from(&[0u8; 64]);
        let mut jar = CookieJar::new();
        let signed = jar.signed_mut(&key);
        assert!(signed.verify(c).is_none());
    }
}

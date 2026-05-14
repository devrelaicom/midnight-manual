//! End-to-end test of the admin challenge-response flow assembled out of
//! Phase 7a primitives.
//!
//! Phase 7b will wrap this in HTTP endpoints; the wire shape changes there,
//! but the call sequence proven here is the contract every HTTP integration
//! test will pin against.

use mn_auth::{
    encode_public_wire, mint_jwt, parse_public_key_wire, verify_jwt, verify_signature,
    ChallengeError, ChallengeStore, Claims, Keypair, Role, SigningSecret, Tier, UserStore,
    DEFAULT_ADMIN_TTL,
};
use time::{Duration, OffsetDateTime};

/// Build a `users.toml` body containing one admin user pinned to the given
/// generated keypair's public key.
fn users_toml_for(user_id: &str, kp: &Keypair) -> String {
    format!(
        r#"
schema_version = 1

[[users]]
user_id = "{user_id}"
role = "admin"
public_key = "{wire}"
created_at = "2026-05-14"
"#,
        wire = kp.public_wire(),
    )
}

fn t0() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_750_000_000).unwrap()
}

fn signing_secret() -> SigningSecret {
    SigningSecret::from_bytes(vec![7u8; 32]).unwrap()
}

#[test]
fn successful_challenge_response_mints_admin_jwt() {
    // Client side: generate a keypair, register it in the (server-side) user
    // store via the wire-encoded public half.
    let kp = Keypair::generate();
    let store = UserStore::parse(&users_toml_for("aaron", &kp)).expect("user store");
    let user = store.get("aaron").expect("aaron exists");
    assert_eq!(user.role, Role::Admin);

    // Server side: mint a challenge for aaron.
    let challenges = ChallengeStore::new();
    let challenge = challenges.mint("aaron", t0(), Duration::seconds(60));

    // Client side: sign the nonce with their private key.
    let signature = kp.sign(&challenge.nonce);

    // Server side: consume the challenge, look up the user, verify the
    // signature against the public key.
    let consumed = challenges
        .consume(&challenge.challenge_id, t0())
        .expect("consume");
    assert_eq!(consumed.user_id, "aaron");
    let public = parse_public_key_wire(&user.public_key).expect("parse public key");
    verify_signature(&public, &consumed.nonce, &signature).expect("signature valid");

    // Server side: mint a JWT and confirm it round-trips.
    let secret = signing_secret();
    let claims = Claims::admin(&consumed.user_id, user.role, t0(), DEFAULT_ADMIN_TTL);
    let token = mint_jwt(&secret, &claims).expect("mint");
    let verified = verify_jwt(&secret, &token, t0()).expect("verify");
    assert_eq!(verified.sub, "aaron");
    assert_eq!(verified.role, Role::Admin);
    assert_eq!(verified.tier, Tier::Admin);
}

#[test]
fn signature_with_wrong_key_is_rejected() {
    let aaron_kp = Keypair::generate();
    let imposter_kp = Keypair::generate();
    let store = UserStore::parse(&users_toml_for("aaron", &aaron_kp)).unwrap();
    let user = store.get("aaron").unwrap();

    let challenges = ChallengeStore::new();
    let c = challenges.mint("aaron", t0(), Duration::seconds(60));

    // Imposter signs aaron's nonce with their own key.
    let bad_signature = imposter_kp.sign(&c.nonce);
    let consumed = challenges.consume(&c.challenge_id, t0()).unwrap();
    let public = parse_public_key_wire(&user.public_key).unwrap();
    let err = verify_signature(&public, &consumed.nonce, &bad_signature).unwrap_err();
    assert!(matches!(err, mn_auth::KeyError::BadSignature));
}

#[test]
fn replay_of_consumed_challenge_id_fails() {
    let challenges = ChallengeStore::new();
    let c = challenges.mint("aaron", t0(), Duration::seconds(60));
    let _first = challenges.consume(&c.challenge_id, t0()).unwrap();
    let err = challenges.consume(&c.challenge_id, t0()).unwrap_err();
    assert_eq!(err, ChallengeError::NotFound, "single-use semantics");
}

#[test]
fn expired_challenge_is_rejected_even_with_correct_signature() {
    let kp = Keypair::generate();
    let challenges = ChallengeStore::new();
    let c = challenges.mint("aaron", t0(), Duration::seconds(60));
    let _signature = kp.sign(&c.nonce);
    let too_late = t0() + Duration::seconds(61);
    assert_eq!(challenges.consume(&c.challenge_id, too_late), Err(ChallengeError::Expired),);
}

#[test]
fn user_store_round_trips_wire_public_key() {
    // The TOML row in the user store must round-trip the same public-key
    // bytes that `parse_public_key_wire` and `encode_public_wire` produce.
    let kp = Keypair::generate();
    let original = kp.verifying().to_bytes();
    let store = UserStore::parse(&users_toml_for("aaron", &kp)).unwrap();
    let user = store.get("aaron").unwrap();
    let parsed = parse_public_key_wire(&user.public_key).unwrap();
    assert_eq!(parsed.to_bytes(), original);

    // And the encoded form must match the stored form exactly.
    let re_encoded = encode_public_wire(&original);
    assert_eq!(re_encoded, kp.public_wire());
    assert_eq!(re_encoded, user.public_key);
}

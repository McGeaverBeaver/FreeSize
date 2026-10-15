<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Hub;

/**
 * Ed25519 keys and the FreeSize protocol's signatures, as the Rust side
 * (`freesize_proto::keys` and `::signing`) makes them: base64 keys and
 * signatures, fingerprints in grouped Crockford base32, requests and
 * envelopes signed over fixed texts.
 */
final class Keys {
	private const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

	public static function newSeed(): string {
		return random_bytes(SODIUM_CRYPTO_SIGN_SEEDBYTES);
	}

	/** The public key of a seed, raw (32 bytes). */
	public static function publicKey(string $seed): string {
		return sodium_crypto_sign_publickey(sodium_crypto_sign_seed_keypair($seed));
	}

	/** A detached signature of `$message`, in base64. */
	public static function sign(string $seed, string $message): string {
		$secret = sodium_crypto_sign_secretkey(sodium_crypto_sign_seed_keypair($seed));
		return base64_encode(sodium_crypto_sign_detached($message, $secret));
	}

	public static function verify(string $public, string $message, string $signature): bool {
		$raw = base64_decode(trim($signature), true);
		if ($raw === false || strlen($raw) !== SODIUM_CRYPTO_SIGN_BYTES || strlen($public) !== SODIUM_CRYPTO_SIGN_PUBLICKEYBYTES) {
			return false;
		}
		try {
			return sodium_crypto_sign_verify_detached($raw, $message, $public);
		} catch (\SodiumException) {
			return false;
		}
	}

	/** A public key from base64, or null. */
	public static function fromBase64(string $text): ?string {
		$raw = base64_decode(trim($text), true);
		return ($raw !== false && strlen($raw) === SODIUM_CRYPTO_SIGN_PUBLICKEYBYTES) ? $raw : null;
	}

	/** `ABCD-EFGH-JKMN-PQRS`: the first 10 bytes of the key's SHA-256. */
	public static function fingerprint(string $public): string {
		$text = self::crockford(substr(hash('sha256', $public, true), 0, 10));
		return implode('-', str_split($text, 4));
	}

	public static function crockford(string $bytes): string {
		$out = '';
		$buffer = 0;
		$bits = 0;
		foreach (str_split($bytes) as $byte) {
			$buffer = (($buffer << 8) | ord($byte)) & 0xFFFF;
			$bits += 8;
			while ($bits >= 5) {
				$bits -= 5;
				$out .= self::CROCKFORD[($buffer >> $bits) & 31];
			}
		}
		if ($bits > 0) {
			$out .= self::CROCKFORD[($buffer << (5 - $bits)) & 31];
		}
		return $out;
	}

	/** A random id like the Hub's: Crockford base32 of `$bytes` random bytes. */
	public static function randomId(int $bytes = 16): string {
		return self::crockford(random_bytes($bytes));
	}

	/** A typed code without dashes or spaces, with look-alike letters fixed. */
	public static function normalizeCode(string $text): string {
		$text = strtoupper(str_replace(['-', ' ', '_'], '', $text));
		return strtr($text, ['O' => '0', 'I' => '1', 'L' => '1']);
	}

	/** `7KQ2-M9XD-4HRT`, or null when it isn't a pairing code. */
	public static function formatCode(string $text): ?string {
		$code = self::normalizeCode($text);
		if (strlen($code) !== 12 || strspn($code, self::CROCKFORD) !== 12) {
			return null;
		}
		return substr($code, 0, 4) . '-' . substr($code, 4, 4) . '-' . substr($code, 8);
	}

	/** The headers of a request signed by a device. */
	public static function requestHeaders(string $seed, string $device, string $method, string $path, string $body, int $time): array {
		$nonce = self::randomId();
		$message = sprintf(
			"disktree-request-v1\n%s\n%s\n%d\n%s\n%s",
			strtoupper($method), $path, $time, $nonce, hash('sha256', $body)
		);
		return [
			'x-disktree-device' => $device,
			'x-disktree-time' => (string)$time,
			'x-disktree-nonce' => $nonce,
			'x-disktree-signature' => self::sign($seed, $message),
		];
	}

	/** An envelope: `{body, signer, signature}` for a purpose such as `decision`. */
	public static function seal(array $value, string $seed, string $purpose): array {
		$body = self::json($value);
		return [
			'body' => $body,
			'signer' => self::fingerprint(self::publicKey($seed)),
			'signature' => self::sign($seed, "disktree-$purpose-v1\n$body"),
		];
	}

	/**
	 * The value inside an envelope signed by `$public` for `$purpose`, or
	 * null when it's someone else's or doesn't verify.
	 */
	public static function open(array $envelope, string $public, string $purpose): ?array {
		$body = $envelope['body'] ?? null;
		if (!is_string($body) || ($envelope['signer'] ?? '') !== self::fingerprint($public)) {
			return null;
		}
		if (!self::verify($public, "disktree-$purpose-v1\n$body", (string)($envelope['signature'] ?? ''))) {
			return null;
		}
		$value = json_decode($body, true);
		return is_array($value) ? $value : null;
	}

	/** An envelope's hash, which a decision names. */
	public static function envelopeHash(array $envelope): string {
		return hash('sha256', (string)($envelope['body'] ?? ''));
	}

	public static function json(mixed $value): string {
		return json_encode($value, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE | JSON_THROW_ON_ERROR);
	}
}

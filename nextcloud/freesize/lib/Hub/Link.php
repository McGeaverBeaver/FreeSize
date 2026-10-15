<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Hub;

/**
 * The dashboard's pairing link, `freesize://pair?hub=…&code=…&key=…`, or
 * a Hub address with a code typed separately (`freesize_proto::pairing`).
 */
final class Link {
	/** @return array{hub: string, code: string, key: ?string}|null */
	public static function parse(string $text, string $code = ''): ?array {
		$text = trim($text);
		foreach (['freesize://pair?', 'disktree://pair?'] as $prefix) {
			if (str_starts_with($text, $prefix)) {
				parse_str(substr($text, strlen($prefix)), $query);
				$hub = self::normalizeHub((string)($query['hub'] ?? ''));
				$typed = Keys::formatCode((string)($query['code'] ?? ''));
				if ($hub === null || $typed === null) {
					return null;
				}
				$key = isset($query['key']) && $query['key'] !== '' ? strtoupper((string)$query['key']) : null;
				return ['hub' => $hub, 'code' => $typed, 'key' => $key];
			}
		}
		$hub = self::normalizeHub($text);
		$typed = Keys::formatCode($code);
		if ($hub === null || $typed === null) {
			return null;
		}
		return ['hub' => $hub, 'code' => $typed, 'key' => null];
	}

	/** `https://hub.lan` from `hub.lan/`; plain http only when written. */
	public static function normalizeHub(string $text): ?string {
		$text = trim($text);
		if (str_starts_with($text, 'http://')) {
			[$scheme, $rest] = ['http', substr($text, 7)];
		} elseif (str_starts_with($text, 'https://')) {
			[$scheme, $rest] = ['https', substr($text, 8)];
		} else {
			[$scheme, $rest] = ['https', $text];
		}
		$rest = rtrim($rest, '/');
		if ($rest === '' || preg_match('/\s/', $rest) || str_contains($rest, '://')) {
			return null;
		}
		return "$scheme://$rest";
	}
}

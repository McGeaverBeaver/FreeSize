<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Hub;

use OCP\Http\Client\IClientService;
use OCP\ITempManager;

/**
 * Talking to the FreeSize Hub, like `freesize_agent::client`: `hello` and
 * `pair` in the open, everything else signed with this device's key. A Hub
 * with its own certificate authority is trusted with that authority, for
 * its address only.
 */
class HubClient {
	public const PROTOCOL = 1;
	private const TIMEOUT = 30;

	public function __construct(
		private IClientService $clients,
		private ITempManager $temp,
	) {
	}

	/** What the Hub at `$hub` says about itself. */
	public function hello(string $hub, ?string $ca): array {
		[$status, $body] = $this->request('GET', "$hub/api/v1/hello", [], null, $ca);
		$hello = $this->decode($status, $body);
		if (($hello['protocol'] ?? 0) !== self::PROTOCOL) {
			throw new HubException('This Hub speaks a different version of the protocol; update FreeSize.');
		}
		if (Keys::fromBase64((string)($hello['key'] ?? '')) === null) {
			throw new HubException("The Hub's answer has no key.");
		}
		return $hello;
	}

	/**
	 * Pairs with the Hub in `$link`: returns the new state's parts. With no
	 * fingerprint in the link, `$expected` must be the Hub's, as the admin
	 * read it off the dashboard.
	 */
	public function pair(array $link, array $info, ?string $expected): array {
		$hub = $link['hub'];
		try {
			$hello = $this->hello($hub, null);
		} catch (HubException $e) {
			// A Hub with its own authority isn't trusted by the system yet:
			// ask it over plain http on the same port, then check its key.
			if (!str_starts_with($hub, 'https://')) {
				throw $e;
			}
			$hello = $this->hello('http://' . substr($hub, 8), null);
		}
		$hubKey = Keys::fromBase64($hello['key']);
		$fingerprint = Keys::fingerprint($hubKey);
		$want = $link['key'] ?? $expected;
		if ($want === null || $want === '') {
			throw new HubException("Compare the Hub's fingerprint $fingerprint with the dashboard's, and pair again with it.", 0, $fingerprint);
		}
		if (strtoupper(trim($want)) !== $fingerprint) {
			throw new HubException("The Hub at $hub has the key $fingerprint, not $want. Something between Nextcloud and the Hub may be pretending to be it.");
		}
		$ca = $hello['ca'] ?? null;
		if ($ca !== null && str_starts_with($hub, 'http://')) {
			$hub = 'https://' . substr($hub, 7);
		}
		$seed = Keys::newSeed();
		$public = Keys::publicKey($seed);
		$request = Keys::seal([
			'code' => $link['code'],
			'key' => base64_encode($public),
			'info' => $info,
			'time' => time(),
		], $seed, 'pair-request');
		[$status, $body] = $this->request('POST', "$hub/api/v1/pair", ['Content-Type' => 'application/json'], Keys::json($request), $ca);
		$signed = $this->decode($status, $body);
		$response = Keys::open($signed, $hubKey, 'pair-response');
		if ($response === null) {
			throw new HubException("The Hub's answer isn't signed by the Hub.");
		}
		if (($response['device_key'] ?? '') !== base64_encode($public) || ($response['hub']['key'] ?? '') !== $hello['key']) {
			throw new HubException("The Hub's answer was meant for another device.");
		}
		if (($response['hub']['ca'] ?? null) !== $ca) {
			throw new HubException("The Hub's signed answer names another certificate authority than it first said.");
		}
		return [
			'hub' => $hub,
			'hub_name' => (string)($hello['name'] ?? 'FreeSize Hub'),
			'hub_key' => $hello['key'],
			'ca' => $ca,
			'device' => (string)$response['device'],
			'seed' => $seed,
		];
	}

	/** A request signed as the paired device; the Hub's answer, decoded. */
	public function signed(State $state, string $method, string $path, ?array $body = null, int $timeout = self::TIMEOUT): array {
		$bytes = $body === null ? '' : Keys::json($body);
		$headers = Keys::requestHeaders($state->seed(), $state->device(), $method, $path, $bytes, time());
		$headers['Accept'] = 'application/json';
		if ($method !== 'GET') {
			$headers['Content-Type'] = 'application/json';
		}
		[$status, $answer] = $this->request($method, $state->hub() . $path, $headers, $method === 'GET' ? null : $bytes, $state->ca(), $timeout);
		return $this->decode($status, $answer);
	}

	/** @return array{int, string} */
	private function request(string $method, string $url, array $headers, ?string $body, ?string $ca, int $timeout = self::TIMEOUT): array {
		$options = [
			'headers' => $headers,
			'timeout' => $timeout,
			'http_errors' => false,
			'allow_redirects' => false,
			// The Hub is usually on the home network.
			'nextcloud' => ['allow_local_address' => true],
		];
		if ($body !== null) {
			$options['body'] = $body;
		}
		if ($ca !== null && str_starts_with($url, 'https://')) {
			$file = $this->temp->getTemporaryFile('.pem');
			file_put_contents($file, $ca);
			$options['verify'] = $file;
		}
		try {
			$client = $this->clients->newClient();
			$response = $method === 'GET' ? $client->get($url, $options) : $client->post($url, $options);
		} catch (\Throwable $e) {
			throw new HubException("Can't reach the Hub: " . $e->getMessage());
		}
		return [$response->getStatusCode(), (string)$response->getBody()];
	}

	private function decode(int $status, string $body): array {
		$value = json_decode($body, true);
		if ($status >= 200 && $status < 300) {
			if (!is_array($value)) {
				throw new HubException("The Hub's answer isn't JSON.");
			}
			return $value;
		}
		$message = is_array($value) && isset($value['error']) ? (string)$value['error'] : "The Hub answered $status.";
		throw new HubException($message, $status, null, is_array($value) && !empty($value['removed']));
	}
}

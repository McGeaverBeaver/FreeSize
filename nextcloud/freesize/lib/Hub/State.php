<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Hub;

use OCA\FreeSize\AppInfo\Application;
use OCP\IAppConfig;
use OCP\Security\ICrypto;

/**
 * The pairing and what goes with it, in Nextcloud's app config, like the
 * agent's `state.json`: the Hub's address, name, key and authority, this
 * device's id and key (encrypted with Nextcloud's secret), the approver set
 * here, grants, the last commands handled, and the users the admin chose.
 */
class State {
	private const MAX_HANDLED = 500;
	private const MAX_GRANT_MINUTES = 8 * 60;

	public function __construct(
		private IAppConfig $config,
		private ICrypto $crypto,
	) {
	}

	private function get(string $key, string $default = ''): string {
		return $this->config->getValueString(Application::APP_ID, $key, $default);
	}

	private function set(string $key, string $value, bool $sensitive = false): void {
		$this->config->setValueString(Application::APP_ID, $key, $value, sensitive: $sensitive);
	}

	private function getJson(string $key): array {
		$value = json_decode($this->get($key, '[]'), true);
		return is_array($value) ? $value : [];
	}

	private function setJson(string $key, array $value): void {
		$this->set($key, Keys::json($value));
	}

	public function paired(): bool {
		return $this->get('device') !== '' && $this->get('seed') !== '';
	}

	public function hub(): string {
		return $this->get('hub');
	}

	public function hubName(): string {
		return $this->get('hub_name');
	}

	/** The Hub's key, raw. */
	public function hubKey(): string {
		return (string)Keys::fromBase64($this->get('hub_key'));
	}

	public function ca(): ?string {
		$ca = $this->get('ca');
		return $ca === '' ? null : $ca;
	}

	public function device(): string {
		return $this->get('device');
	}

	public function seed(): string {
		return $this->crypto->decrypt($this->get('seed'));
	}

	public function fingerprint(): string {
		return $this->paired() ? Keys::fingerprint(Keys::publicKey($this->seed())) : '';
	}

	/** Saves a new pairing from `HubClient::pair`, forgetting the old one. */
	public function savePairing(array $pairing): void {
		$this->forget();
		$this->set('hub', $pairing['hub']);
		$this->set('hub_name', $pairing['hub_name']);
		$this->set('hub_key', $pairing['hub_key']);
		$this->set('ca', $pairing['ca'] ?? '');
		$this->set('device', $pairing['device']);
		$this->set('seed', $this->crypto->encrypt($pairing['seed']), true);
		// Requests shown before are sent again on the first poll.
		$this->set('resend', '1');
	}

	public function forget(): void {
		foreach (['hub', 'hub_name', 'hub_key', 'ca', 'device', 'seed', 'approver', 'grants', 'handled', 'waiting', 'reported', 'scanned', 'resend', 'last_report', 'last_poll', 'last_error'] as $key) {
			$this->config->deleteKey(Application::APP_ID, $key);
		}
	}

	public function setHub(string $hub, ?string $ca): void {
		$this->set('hub', $hub);
		$this->set('ca', $ca ?? '');
	}

	/** @return array{device: string, name: string, key: string}|null The key raw. */
	public function approver(): ?array {
		$approver = $this->getJson('approver');
		if (!isset($approver['device'], $approver['key'])) {
			return null;
		}
		$approver['key'] = (string)Keys::fromBase64($approver['key']);
		return $approver;
	}

	public function approverFingerprint(): ?string {
		$approver = $this->approver();
		return $approver === null ? null : Keys::fingerprint($approver['key']);
	}

	/** Sets the approver from the Hub's `DeviceKey`; null removes it. */
	public function setApprover(?array $deviceKey): void {
		if ($deviceKey === null) {
			$this->config->deleteKey(Application::APP_ID, 'approver');
			return;
		}
		$this->setJson('approver', [
			'device' => (string)$deviceKey['id'],
			'name' => (string)$deviceKey['name'],
			'key' => (string)$deviceKey['key'],
		]);
	}

	/** Grants still running, as the protocol's `Grant`s. */
	public function grants(int $now): array {
		return array_values(array_filter($this->getJson('grants'), fn ($g) => ($g['expires'] ?? 0) > $now));
	}

	public function grantFor(string $admin, string $kind, int $now): ?array {
		foreach ($this->grants($now) as $grant) {
			if ($grant['admin'] === $admin && $grant['kind'] === $kind) {
				return $grant;
			}
		}
		return null;
	}

	public function addGrant(string $admin, string $adminName, string $kind, int $minutes, int $now): void {
		$minutes = min($minutes, self::MAX_GRANT_MINUTES);
		$grants = array_filter($this->grants($now), fn ($g) => !($g['admin'] === $admin && $g['kind'] === $kind));
		$grants[] = ['admin' => $admin, 'admin_name' => $adminName, 'kind' => $kind, 'expires' => $now + $minutes * 60];
		$this->setJson('grants', array_values($grants));
	}

	public function endGrants(): int {
		$ended = count($this->grants(time()));
		$this->config->deleteKey(Application::APP_ID, 'grants');
		return $ended;
	}

	/** The name the Hub shows; "Nextcloud" unless the admin chose another. */
	public function name(): string {
		return $this->get('name', 'Nextcloud') ?: 'Nextcloud';
	}

	public function setName(string $name): void {
		$name = trim($name);
		$this->set('name', $name === '' ? 'Nextcloud' : mb_substr($name, 0, 60));
	}

	public function wasHandled(string $id): bool {
		return in_array($id, $this->getJson('handled'), true);
	}

	public function markHandled(string $id): void {
		$handled = $this->getJson('handled');
		$handled[] = $id;
		$this->setJson('handled', array_slice(array_values(array_unique($handled)), -self::MAX_HANDLED));
		$this->dropWaiting($id);
	}

	/** Commands waiting for the approver: id => the signed command. */
	public function waiting(): array {
		return $this->getJson('waiting');
	}

	public function addWaiting(string $id, array $signed): void {
		$waiting = $this->waiting();
		$waiting[$id] = $signed;
		$this->setJson('waiting', $waiting);
	}

	public function dropWaiting(string $id): void {
		$waiting = $this->waiting();
		if (isset($waiting[$id])) {
			unset($waiting[$id]);
			$this->setJson('waiting', $waiting);
		}
	}

	/** @return string[] The users covered. */
	public function users(): array {
		return array_values(array_filter($this->getJson('users'), 'is_string'));
	}

	public function setUsers(array $users): void {
		$this->setJson('users', array_values(array_unique(array_map('strval', $users))));
	}

	/** Takes the "send everything again" mark of a fresh pairing. */
	public function takeResend(): bool {
		$resend = $this->get('resend') === '1';
		if ($resend) {
			$this->config->deleteKey(Application::APP_ID, 'resend');
		}
		return $resend;
	}

	/** The hash of what was last reported, so an unchanged report isn't sent. */
	public function reported(): string {
		return $this->get('reported');
	}

	public function setReported(string $hash): void {
		$this->set('reported', $hash);
	}

	/** What the users' files looked like at the last full read (`Scanner::etags`). */
	public function scanned(): string {
		return $this->get('scanned');
	}

	public function setScanned(string $etags): void {
		$this->set('scanned', $etags);
	}

	public function status(): array {
		return [
			'last_report' => (int)$this->get('last_report', '0'),
			'last_poll' => (int)$this->get('last_poll', '0'),
			'last_error' => $this->get('last_error'),
		];
	}

	public function setStatus(string $key, string $value): void {
		$this->set($key, $value);
	}
}

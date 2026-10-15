<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Service;

use OCA\FreeSize\AppInfo\Application;
use OCA\FreeSize\Hub\HubClient;
use OCA\FreeSize\Hub\HubException;
use OCA\FreeSize\Hub\Keys;
use OCA\FreeSize\Hub\Link;
use OCA\FreeSize\Hub\State;
use OCP\App\IAppManager;
use OCP\IConfig;
use OCP\Lock\ILockingProvider;
use OCP\Lock\LockedException;
use Psr\Log\LoggerInterface;

/**
 * The FreeSize agent inside Nextcloud, like `freesize agent run`: it
 * reports what the covered users' files take, and carries out deletes the
 * Hub asks for once this device's approver allowed them on their phone (or
 * a grant they gave covers them). Nobody sits at a Nextcloud server, so
 * nothing runs without the approver.
 */
class Agent {
	/** How long one run may spend fingerprinting files. */
	private const HASH_SECONDS = 120;
	/** A report that didn't change still goes out this often. */
	private const REPORT_EVERY = 6 * 60 * 60;
	/** While a request waits for the approver, a run listens this long for the answer. */
	private const LISTEN_SECONDS = 120;
	private const LOCK = 'freesize/run';

	public function __construct(
		private State $state,
		private HubClient $hub,
		private Scanner $scanner,
		private Deleter $deleter,
		private Hasher $hasher,
		private IAppManager $apps,
		private IConfig $config,
		private ILockingProvider $locks,
		private LoggerInterface $logger,
	) {
	}

	public function state(): State {
		return $this->state;
	}

	public function trashBin(): bool {
		return $this->apps->isEnabledForAnyone('files_trashbin');
	}

	/** What the Hub shows about this device, as `DeviceInfo`. */
	public function info(): array {
		$url = (string)$this->config->getSystemValueString('overwrite.cli.url', '');
		$host = (string)(parse_url($url, PHP_URL_HOST) ?: gethostname());
		$name = $this->state->name();
		return [
			'name' => $name,
			'platform' => 'nextcloud',
			'os' => 'Nextcloud ' . $this->config->getSystemValueString('version', ''),
			'hostname' => $host,
			'app' => 'FreeSize Nextcloud app ' . Application::VERSION,
			'actions' => ['delete'],
			'attended' => false,
			'approver' => $this->state->approverFingerprint(),
			'grants' => $this->state->grants(time()),
			'rights' => '',
		];
	}

	/** Pairs with the Hub in `$link` (or the Hub `$link` with `$code`). */
	public function pair(string $link, string $code, ?string $fingerprint): string {
		$parsed = Link::parse($link, $code);
		if ($parsed === null) {
			throw new HubException('Paste the link from the Hub\'s "Pair a device", or the Hub\'s address and its code.');
		}
		$pairing = $this->hub->pair($parsed, $this->info(), $fingerprint);
		$this->state->savePairing($pairing);
		$this->log("paired with {$pairing['hub_name']} at {$pairing['hub']}");
		return $pairing['hub_name'];
	}

	/** Forgets the Hub; it shows the device until it's removed there too. */
	public function unpair(): void {
		$this->state->forget();
	}

	/** Ends every grant, so each request is asked about again. */
	public function endGrants(): int {
		$ended = $this->state->endGrants();
		if ($this->state->paired()) {
			$this->sendInfo();
		}
		return $ended;
	}

	/** Renames this device on the Hub. */
	public function rename(string $name): void {
		$this->state->setName($name);
		if ($this->state->paired()) {
			$this->sendInfo();
		}
	}

	/**
	 * Sets the approver from the fingerprint shown on their phone; null or
	 * "" removes it. Only here, never from the Hub.
	 */
	public function setApprover(?string $fingerprint): ?string {
		if ($fingerprint === null || trim($fingerprint) === '') {
			$this->state->setApprover(null);
			$this->sendInfo();
			return null;
		}
		$fingerprint = strtoupper(trim($fingerprint));
		$key = $this->hub->signed($this->state, 'GET', '/api/v1/devices/' . rawurlencode($fingerprint));
		$raw = Keys::fromBase64((string)($key['key'] ?? ''));
		if ($raw === null || Keys::fingerprint($raw) !== $fingerprint) {
			throw new HubException("The Hub's answer doesn't match the fingerprint $fingerprint.");
		}
		if (($key['id'] ?? '') === $this->state->device()) {
			throw new HubException('Choose the phone of the person who approves, not this Nextcloud.');
		}
		$this->state->setApprover($key);
		$this->sendInfo();
		$this->log("requests are approved by {$key['name']} ($fingerprint)");
		return (string)$key['name'];
	}

	private function sendInfo(): void {
		try {
			$this->hub->signed($this->state, 'POST', '/api/v1/report', ['info' => $this->info()]);
		} catch (HubException $e) {
			$this->state->setStatus('last_error', $e->getMessage());
		}
	}

	/**
	 * One run of the background job: report when something changed, then
	 * take what the Hub has for this device. With `$force` the report goes
	 * out whatever.
	 */
	public function run(bool $force = false): string {
		if (!$this->state->paired()) {
			return 'not paired with a Hub';
		}
		try {
			$this->locks->acquireLock(self::LOCK, ILockingProvider::LOCK_EXCLUSIVE);
		} catch (LockedException) {
			return 'already running';
		}
		try {
			$said = [];
			$said[] = $this->report($force);
			$said[] = $this->poll(0);
			$deadline = time() + self::LISTEN_SECONDS;
			while ($this->state->waiting() !== [] && time() < $deadline) {
				$said[] = $this->poll(min(30, max(1, $deadline - time())));
			}
			$this->state->setStatus('last_error', '');
			return implode('; ', array_filter($said));
		} catch (HubException $e) {
			if ($e->removed) {
				$this->log('the Hub removed this device: forgetting the pairing');
				$this->state->forget();
				return 'the Hub removed this device';
			}
			$this->state->setStatus('last_error', $e->getMessage());
			$this->log('the Hub: ' . $e->getMessage());
			return $e->getMessage();
		} finally {
			$this->locks->releaseLock(self::LOCK, ILockingProvider::LOCK_EXCLUSIVE);
		}
	}

	/** Sends the report when it changed, or now and then anyway. */
	public function report(bool $force): string {
		$now = time();
		if ($now % 86400 < 600) {
			$this->hasher->prune();
		}
		$users = $this->state->users();
		// Reading every user's file list is skipped while nothing changed
		// and nothing is left to fingerprint.
		$etags = hash('sha256', Keys::json([$users, $this->info(), $this->scanner->etags($users)]));
		$last = $this->state->status()['last_report'];
		if (!$force && $etags === $this->state->scanned() && $now - $last < self::REPORT_EVERY) {
			return '';
		}
		$cloud = $this->scanner->report($users, $this->trashBin(), self::HASH_SECONDS, $now);
		$hashing = array_filter($cloud['users'], fn ($u) => $u['hashed'] < $u['to_hash']) !== [];
		$same = $cloud;
		$same['taken'] = 0;
		$hash = hash('sha256', Keys::json([$same, $this->info()]));
		if ($force || $hash !== $this->state->reported() || $now - $last >= self::REPORT_EVERY) {
			$this->hub->signed($this->state, 'POST', '/api/v1/report', ['info' => $this->info(), 'cloud' => $cloud]);
			$this->state->setReported($hash);
			$this->state->setStatus('last_report', (string)$now);
			$said = 'reported ' . count($cloud['users']) . ' users';
		}
		// Still fingerprinting: read again next time.
		$this->state->setScanned($hashing ? '' : $etags);
		return $said ?? '';
	}


	/** Takes what the Hub has for this device and acts on it. */
	public function poll(int $wait): string {
		$response = $this->hub->signed(
			$this->state,
			'POST',
			'/api/v1/poll',
			['wait' => $wait, 'resend' => $this->state->takeResend()],
			$wait + 30,
		);
		$this->state->setStatus('last_poll', (string)time());
		if (isset($response['move_to']) && is_array($response['move_to'])) {
			$this->moveTo($response['move_to']);
		}
		return $this->handle($response);
	}

	/** Follows the Hub to its new address once it answers there with its key. */
	private function moveTo(array $signed): void {
		$address = Keys::open($signed, $this->state->hubKey(), 'move');
		if ($address === null || !isset($address['url'])) {
			return;
		}
		$hello = $this->hub->hello((string)$address['url'], $address['ca'] ?? null);
		if (($hello['key'] ?? '') !== base64_encode($this->state->hubKey())) {
			return;
		}
		$this->state->setHub((string)$address['url'], $address['ca'] ?? null);
		$this->log("the Hub is at {$address['url']} now");
	}

	public function handle(array $response): string {
		$now = time();
		$said = [];
		foreach ((array)($response['cancelled'] ?? []) as $id) {
			if (isset($this->state->waiting()[$id])) {
				$this->state->dropWaiting((string)$id);
				$this->log("request $id was withdrawn");
			}
		}
		foreach ($this->state->waiting() as $id => $signed) {
			$command = Keys::open($signed, $this->state->hubKey(), 'command');
			if ($command === null || $command['expires'] <= $now) {
				$this->state->markHandled((string)$id);
				$this->update((string)$id, 'expired', "the approver didn't answer in time");
			}
		}
		foreach ((array)($response['commands'] ?? []) as $signed) {
			$said[] = $this->consider((array)$signed, $now);
		}
		foreach ((array)($response['decisions'] ?? []) as $signed) {
			$said[] = $this->decided((array)$signed, $now);
		}
		return implode('; ', array_filter($said));
	}

	/** A command from the Hub: refuse it, run it under a grant, or ask the approver. */
	private function consider(array $signed, int $now): string {
		$command = Keys::open($signed, $this->state->hubKey(), 'command');
		if ($command === null || ($command['device'] ?? '') !== $this->state->device()) {
			// Not from our Hub, or not for us: don't even answer it.
			return '';
		}
		$id = (string)$command['id'];
		if ($this->state->wasHandled($id) || isset($this->state->waiting()[$id])) {
			return '';
		}
		$refuse = function (string $state, string $reason) use ($id): string {
			$this->state->markHandled($id);
			$this->update($id, $state, $reason);
			$this->log("refused request $id: $reason");
			return "refused $id";
		};
		if ((int)$command['expires'] <= $now) {
			return $refuse('expired', 'the request expired');
		}
		$action = (array)($command['action'] ?? []);
		if (($action['type'] ?? '') !== 'delete') {
			return $refuse('denied', 'the Nextcloud app only deletes files, into the trash bin');
		}
		$problem = $this->deleter->check($action, $this->state->users(), $this->trashBin());
		if ($problem !== null) {
			return $refuse('denied', $problem);
		}
		$grant = $this->state->grantFor((string)$command['by']['subject'], 'delete', $now);
		if ($grant !== null) {
			return $this->carryOut($id, $action, ['by' => ['type' => 'grant', 'expires' => $grant['expires']], 'minutes' => 0]);
		}
		if ($this->state->approver() === null) {
			return $refuse('denied', 'no approver is set: choose one in Nextcloud\'s admin settings under FreeSize, or with `occ freesize:approver`');
		}
		$this->state->addWaiting($id, $signed);
		$this->update($id, 'needs_approver', $this->deleter->note($action));
		$this->log("asking the approver about request $id");
		return "asked the approver about $id";
	}

	/** The approver's answer to a request waiting here. */
	private function decided(array $signed, int $now): string {
		$peek = json_decode((string)($signed['body'] ?? ''), true);
		$id = is_array($peek) ? (string)($peek['command'] ?? '') : '';
		$waiting = $this->state->waiting()[$id] ?? null;
		$approver = $this->state->approver();
		if ($waiting === null || $approver === null) {
			return '';
		}
		$decision = Keys::open($signed, $approver['key'], 'decision');
		if ($decision === null) {
			$this->log("ignored a decision about request $id: it isn't from this device's approver");
			return '';
		}
		if (($decision['command'] ?? '') !== $id || ($decision['device'] ?? '') !== $this->state->device()
			|| ($decision['command_hash'] ?? '') !== Keys::envelopeHash($waiting)) {
			$this->log("ignored a decision about request $id: it is about a different request");
			return '';
		}
		$command = Keys::open($waiting, $this->state->hubKey(), 'command');
		if ($command === null) {
			return '';
		}
		if (empty($decision['allow'])) {
			$this->state->markHandled($id);
			$this->update($id, 'denied', 'denied by the approver');
			$this->log("the approver denied request $id");
			return "denied $id";
		}
		$minutes = max(0, (int)($decision['minutes'] ?? 0));
		if ($minutes > 0) {
			$this->state->addGrant((string)$command['by']['subject'], (string)$command['by']['name'], 'delete', $minutes, $now);
		}
		$fingerprint = Keys::fingerprint($approver['key']);
		return $this->carryOut($id, (array)$command['action'], ['by' => ['type' => 'approver', 'fingerprint' => $fingerprint], 'minutes' => min($minutes, 8 * 60)]);
	}

	private function carryOut(string $id, array $action, array $approval): string {
		$this->state->markHandled($id);
		$this->update($id, 'running', 'moving to the trash bin', $approval);
		$result = $this->deleter->delete($action, $this->state->users(), $this->trashBin());
		$state = $result['deleted'] > 0 ? 'done' : 'failed';
		$this->update($id, $state, $result['detail'], $approval, $result['freed'], $result['failed']);
		$this->log("request $id: {$result['detail']}");
		// The report shows what's left.
		$this->state->setReported('');
		$this->state->setScanned('');
		return "request $id $state";
	}

	private function update(string $id, string $state, string $detail, ?array $approval = null, int $freed = 0, array $failed = []): void {
		try {
			$this->hub->signed($this->state, 'POST', '/api/v1/commands/' . rawurlencode($id), [
				'state' => $state,
				'detail' => $detail,
				'approval' => $approval,
				'freed' => $freed,
				'failed' => $failed,
			]);
		} catch (HubException $e) {
			$this->log("couldn't tell the Hub about request $id: " . $e->getMessage());
		}
	}

	private function log(string $message): void {
		$this->logger->info('FreeSize: ' . $message, ['app' => Application::APP_ID]);
	}
}

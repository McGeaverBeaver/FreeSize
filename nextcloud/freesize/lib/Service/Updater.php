<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Service;

use OCA\FreeSize\AppInfo\Application;
use OCA\FreeSize\Hub\Keys;
use OCA\FreeSize\Hub\State;
use OCP\App\IAppManager;
use OCP\Http\Client\IClientService;
use OCP\ITempManager;
use Psr\Log\LoggerInterface;

/**
 * Updating the app from a FreeSize release on GitHub, the way the desktop
 * apps update themselves: the release's SHA256SUMS.txt must be signed by
 * the release key this copy carries (release-key.txt, put there when the
 * app was packed), and the tarball must match the sums. Then the app's
 * folder is replaced and Nextcloud runs the app's upgrade, as `occ
 * app:update` would. Nothing updates on its own: an admin asks for it on
 * the settings page or with `occ freesize:update`.
 */
class Updater {
	public const REPOSITORY = 'McGeaverBeaver/freesize';
	private const RELEASES = 'https://api.github.com/repos/' . self::REPOSITORY . '/releases?per_page=20';
	/** What a release signature covers, as the Rust side signs it. */
	private const DOMAIN = "freesize-release-v1\n";
	private const APP = 'freesize';

	public function __construct(
		private State $state,
		private IClientService $clients,
		private ITempManager $temp,
		private IAppManager $apps,
		private LoggerInterface $logger,
	) {
	}

	// ----- Pure helpers, checked by tests/run.php ---------------------------

	/** The numbers in "0.9.1", "v0.9.1" or "FreeSize app 0.9.1". */
	public static function versionNumber(string $text): ?array {
		$words = preg_split('/\s+/', trim($text)) ?: [];
		$word = ltrim((string)end($words), 'v');
		if (!preg_match('/^(\d+)\.(\d+)(?:\.(\d+))?/', $word, $m)) {
			return null;
		}
		return [(int)$m[1], (int)$m[2], (int)($m[3] ?? 0)];
	}

	public static function newer(string $candidate, string $current): bool {
		$a = self::versionNumber($candidate);
		$b = self::versionNumber($current);
		return $a !== null && $b !== null && $a > $b;
	}

	/** The key in a public-key.txt: the first line that isn't a comment, base64. */
	public static function keyIn(string $text): ?string {
		foreach (preg_split('/\R/', $text) ?: [] as $line) {
			$line = trim($line);
			if ($line === '' || str_starts_with($line, '#')) {
				continue;
			}
			$raw = base64_decode($line, true);
			return $raw !== false && strlen($raw) === SODIUM_CRYPTO_SIGN_PUBLICKEYBYTES ? $raw : null;
		}
		return null;
	}

	/** The release key this copy trusts, or null when it was packed without one. */
	public static function releaseKey(): ?string {
		foreach ([__DIR__ . '/../../release-key.txt', __DIR__ . '/../../../../release/public-key.txt'] as $file) {
			if (is_file($file)) {
				return self::keyIn((string)file_get_contents($file));
			}
		}
		return null;
	}

	/**
	 * The newest published release newer than $current that has the app's
	 * tarball, from GitHub's releases JSON: version, page, the tarball's
	 * name and address, and the sums file and its signature when there.
	 */
	public static function pick(array $releases, string $current): ?array {
		$best = null;
		foreach ($releases as $release) {
			if (!is_array($release) || !empty($release['draft']) || !empty($release['prerelease'])) {
				continue;
			}
			$version = ltrim((string)($release['tag_name'] ?? ''), 'v');
			if (!self::newer($version, $current) || ($best !== null && !self::newer($version, $best['version']))) {
				continue;
			}
			$assets = [];
			foreach ((array)($release['assets'] ?? []) as $asset) {
				if (is_array($asset) && isset($asset['name'], $asset['browser_download_url'])) {
					$assets[(string)$asset['name']] = (string)$asset['browser_download_url'];
				}
			}
			$name = 'freesize-nextcloud-' . $version . '.tar.gz';
			if (!isset($assets[$name])) {
				continue;
			}
			$best = [
				'version' => $version,
				'url' => (string)($release['html_url'] ?? 'https://github.com/' . self::REPOSITORY . '/releases'),
				'asset' => $name,
				'download' => $assets[$name],
				'sums' => $assets['SHA256SUMS.txt'] ?? null,
				'signature' => $assets['SHA256SUMS.txt.sig'] ?? null,
			];
		}
		return $best;
	}

	/** The hex SHA-256 the sums file has for $name, or null. */
	public static function sumFor(string $sums, string $name): ?string {
		foreach (preg_split('/\R/', $sums) ?: [] as $line) {
			if (preg_match('/^([0-9a-fA-F]{64})\s+\*?(.+)$/', trim($line), $m) && trim($m[2]) === $name) {
				return strtolower($m[1]);
			}
		}
		return null;
	}

	/** Whether a tarball entry is where the app's files belong: `freesize/…`, with nothing odd. */
	public static function safeEntry(string $name): bool {
		$name = str_replace('\\', '/', $name);
		if ($name === '' || str_starts_with($name, '/') || str_contains($name, "\0")) {
			return false;
		}
		$parts = explode('/', rtrim($name, '/'));
		if ($parts[0] !== self::APP) {
			return false;
		}
		foreach ($parts as $part) {
			if ($part === '' || $part === '.' || $part === '..') {
				return false;
			}
		}
		return true;
	}

	// ----- Knowing about a newer release ------------------------------------

	/** The Hub said a newer release is out (in a poll answer): remember it. */
	public function learned(array $latest): void {
		$version = ltrim((string)($latest['version'] ?? ''), 'v');
		if ($version !== '' && self::newer($version, Application::VERSION)) {
			$this->state->setStatus('latest_version', $version);
			$this->state->setStatus('latest_url', (string)($latest['url'] ?? ''));
		}
	}

	/** Asks GitHub now; remembers what it found, or that there's nothing newer. */
	public function check(): ?array {
		$releases = $this->fetchJson(self::RELEASES);
		$found = self::pick($releases, Application::VERSION);
		$this->state->setStatus('latest_version', $found['version'] ?? '');
		$this->state->setStatus('latest_url', $found['url'] ?? '');
		return $found;
	}

	/** What the settings page needs to know: whether this copy can update itself, and why not. */
	public function situation(): array {
		$path = $this->appPath();
		return [
			'key' => self::releaseKey() !== null,
			'path' => $path,
			'linked' => $path !== '' && is_link($path),
			'writable' => $path !== '' && is_writable($path) && is_writable(dirname($path)),
		];
	}

	// ----- Doing it ---------------------------------------------------------

	/**
	 * Updates to the newest release on GitHub, or from a tarball an admin
	 * put on the server ($file: checked for what it is, not for a
	 * signature, like unpacking it by hand). Returns a sentence.
	 *
	 * @throws \RuntimeException with the reason it couldn't
	 */
	public function update(?string $file = null): string {
		$this->canReplace();
		if ($file !== null) {
			if (!is_file($file) || !is_readable($file)) {
				throw new \RuntimeException("There's no readable file at $file.");
			}
			return $this->install($file, null);
		}
		$key = self::releaseKey();
		if ($key === null) {
			throw new \RuntimeException("This copy of the app carries no release key, so it can't check what it downloads. Update by hand: unpack the release's freesize-nextcloud-<version>.tar.gz over " . $this->appPath() . ' and run `occ upgrade`, or `occ freesize:update --file FILE`.');
		}
		$latest = $this->check();
		if ($latest === null) {
			return 'This is the newest version (' . Application::VERSION . ').';
		}
		if ($latest['sums'] === null || $latest['signature'] === null) {
			throw new \RuntimeException("Release {$latest['version']} isn't signed, so it's not installed from here. Install it by hand if you trust it.");
		}
		$sums = $this->fetch($latest['sums']);
		$signature = $this->fetch($latest['signature']);
		if (!Keys::verify($key, self::DOMAIN . $sums, $signature)) {
			throw new \RuntimeException("Release {$latest['version']}'s sums file is NOT signed by the release key: not installing it.");
		}
		$want = self::sumFor($sums, $latest['asset']);
		if ($want === null) {
			throw new \RuntimeException("Release {$latest['version']}'s sums file doesn't list {$latest['asset']}.");
		}
		$tar = $this->temp->getTemporaryFile('.tar.gz');
		if ($tar === false) {
			throw new \RuntimeException("Couldn't make a temporary file.");
		}
		$this->fetch($latest['download'], $tar);
		if (hash_file('sha256', $tar) !== $want) {
			throw new \RuntimeException("The downloaded {$latest['asset']} doesn't match the release's sums: not installing it.");
		}
		return $this->install($tar, $latest['version']);
	}

	/** Refuses early when the app's folder can't be swapped. */
	private function canReplace(): void {
		$path = $this->appPath();
		if ($path === '') {
			throw new \RuntimeException("Nextcloud doesn't know where the app is installed.");
		}
		if (is_link($path)) {
			throw new \RuntimeException("$path is a link to " . (string)readlink($path) . ', as on a development setup: update that checkout instead.');
		}
		if (!is_writable($path) || !is_writable(dirname($path))) {
			throw new \RuntimeException("Nextcloud can't write to " . dirname($path) . ", so it can't replace the app's files. Update by hand: unpack the release's freesize-nextcloud-<version>.tar.gz over $path and run `occ upgrade`.");
		}
	}

	/** Unpacks a tarball, checks it's the app, swaps the folder and runs the app's upgrade. */
	private function install(string $tar, ?string $expected): string {
		$dir = $this->temp->getTemporaryFolder();
		if ($dir === false) {
			throw new \RuntimeException("Couldn't make a temporary folder.");
		}
		$dir = rtrim($dir, '/');
		try {
			$archive = new \PharData($tar);
			$entries = new \RecursiveIteratorIterator($archive, \RecursiveIteratorIterator::SELF_FIRST);
			foreach ($entries as $entry) {
				$name = $entries->getSubPathname();
				if (!self::safeEntry($name)) {
					throw new \RuntimeException("The tarball holds $name, which isn't part of the app: not installing it.");
				}
			}
			$archive->extractTo($dir, null, true);
		} catch (\RuntimeException $e) {
			throw $e;
		} catch (\Throwable $e) {
			throw new \RuntimeException("Couldn't unpack the tarball: " . $e->getMessage());
		}
		$info = @simplexml_load_string((string)@file_get_contents("$dir/" . self::APP . '/appinfo/info.xml'));
		if ($info === false || (string)$info->id !== self::APP) {
			throw new \RuntimeException("That tarball isn't the FreeSize app.");
		}
		$version = (string)$info->version;
		if ($expected !== null && $version !== $expected) {
			throw new \RuntimeException("The tarball says version $version, not $expected.");
		}
		if (version_compare($version, Application::VERSION) < 0) {
			throw new \RuntimeException("That's version $version, older than this one (" . Application::VERSION . '): not going back.');
		}
		$path = $this->appPath();
		$old = dirname($path) . '/' . self::APP . '.old-' . time();
		if (!@rename($path, $old)) {
			throw new \RuntimeException("Couldn't move the app's folder aside.");
		}
		if (!@rename("$dir/" . self::APP, $path)) {
			// The temporary folder is on another filesystem: copy instead.
			try {
				self::copyTree("$dir/" . self::APP, $path);
			} catch (\Throwable $e) {
				self::removeTree($path);
				@rename($old, $path);
				throw new \RuntimeException("Couldn't put the new files in place: " . $e->getMessage());
			}
		}
		self::removeTree($old);
		self::removeTree($dir);
		if (function_exists('opcache_reset')) {
			@opcache_reset();
		}
		$upgraded = \OC_App::updateApp(self::APP);
		$this->logger->info("FreeSize updated to $version" . ($upgraded ? '' : ' (its upgrade step reported a problem)'));
		if (ltrim($this->state->status()['latest_version'], 'v') === $version) {
			$this->state->setStatus('latest_version', '');
			$this->state->setStatus('latest_url', '');
		}
		return "Updated to $version." . ($upgraded ? '' : " Nextcloud's upgrade step for the app reported a problem: run `occ upgrade`.");
	}

	private function appPath(): string {
		try {
			return rtrim($this->apps->getAppPath(self::APP), '/');
		} catch (\Throwable) {
			return '';
		}
	}

	private function fetchJson(string $url): array {
		$body = $this->fetch($url);
		$data = json_decode($body, true);
		if (!is_array($data)) {
			throw new \RuntimeException("GitHub didn't answer with the releases.");
		}
		return $data;
	}

	/** GETs a URL; the body, or into $sink. */
	private function fetch(string $url, ?string $sink = null): string {
		$options = [
			'timeout' => 120,
			'headers' => ['Accept' => $sink === null && str_contains($url, 'api.github.com') ? 'application/vnd.github+json' : '*/*'],
		];
		if ($sink !== null) {
			$options['sink'] = $sink;
		}
		try {
			$response = $this->clients->newClient()->get($url, $options);
		} catch (\Throwable $e) {
			throw new \RuntimeException("Couldn't fetch $url: " . $e->getMessage());
		}
		return $sink === null ? (string)$response->getBody() : '';
	}

	private static function copyTree(string $from, string $to): void {
		if (!@mkdir($to, 0755, true) && !is_dir($to)) {
			throw new \RuntimeException("couldn't make $to");
		}
		foreach (new \DirectoryIterator($from) as $entry) {
			if ($entry->isDot()) {
				continue;
			}
			$target = $to . '/' . $entry->getFilename();
			if ($entry->isDir()) {
				self::copyTree($entry->getPathname(), $target);
			} elseif (!@copy($entry->getPathname(), $target)) {
				throw new \RuntimeException("couldn't copy " . $entry->getFilename());
			}
		}
	}

	private static function removeTree(string $path): void {
		if (is_link($path) || is_file($path)) {
			@unlink($path);
			return;
		}
		if (!is_dir($path)) {
			return;
		}
		foreach (new \DirectoryIterator($path) as $entry) {
			if (!$entry->isDot()) {
				self::removeTree($entry->getPathname());
			}
		}
		@rmdir($path);
	}
}

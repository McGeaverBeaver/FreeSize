<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Service;

use OCP\Files\IMimeTypeLoader;
use OCP\Files\IRootFolder;
use OCP\IConfig;
use OCP\IDBConnection;
use OCP\IUser;
use OCP\IUserManager;

/**
 * What each covered user's files take, from Nextcloud's file cache, as the
 * Hub's `freesize_proto::cloud::CloudUser`. Only the user's own files, in
 * their home storage: what others share with them and external storage
 * belong to someone else and are left out.
 */
class Scanner {
	public const MAX_FILES = 100;
	public const MAX_SETS = 100;
	public const MAX_FOLDERS = 500;
	public const MIN_DUPLICATE = 1024 * 1024;
	public const OLD_AFTER = 365 * 24 * 60 * 60;

	public function __construct(
		private IDBConnection $db,
		private IRootFolder $root,
		private IUserManager $users,
		private IMimeTypeLoader $mimeTypes,
		private IConfig $config,
		private Hasher $hasher,
	) {
	}

	/**
	 * The report for `$ids`; fingerprints what it can in `$seconds`.
	 *
	 * @param string[] $ids
	 */
	public function report(array $ids, bool $trashBin, int $seconds, int $now): array {
		$deadline = microtime(true) + $seconds;
		$users = [];
		foreach ($ids as $id) {
			$user = $this->users->get($id);
			if ($user === null) {
				continue;
			}
			$scanned = $this->user($user, $deadline, $now);
			if ($scanned !== null) {
				$users[] = $scanned;
			}
		}
		return [
			'taken' => $now,
			'server' => rtrim((string)$this->config->getSystemValueString('overwrite.cli.url', ''), '/'),
			'version' => (string)$this->config->getSystemValueString('version', ''),
			'trash_bin' => $trashBin,
			'users' => $users,
		];
	}

	/**
	 * What changes whenever a user's files, trash bin or old versions do:
	 * Nextcloud passes a new etag up to every folder above a change.
	 *
	 * @param string[] $ids
	 */
	public function etags(array $ids): string {
		$tags = [];
		foreach ($ids as $id) {
			$home = $this->home($id);
			if ($home === null) {
				continue;
			}
			$query = $this->db->getQueryBuilder();
			$query->select('path', 'etag', 'size')
				->from('filecache')
				->where($query->expr()->eq('storage', $query->createNamedParameter($home[1])))
				->andWhere($query->expr()->in('path', $query->createNamedParameter(['files', 'files_trashbin', 'files_versions'], \OCP\DB\QueryBuilder\IQueryBuilder::PARAM_STR_ARRAY)));
			$result = $query->executeQuery();
			while ($row = $result->fetch()) {
				$tags[] = "$id/{$row['path']}:{$row['etag']}:{$row['size']}";
			}
			$result->closeCursor();
		}
		sort($tags);
		return hash('sha256', implode("\n", $tags));
	}

	/** The user's numeric storage and their files folder, or null without one. */
	public function home(string $id): ?array {
		try {
			$folder = $this->root->getUserFolder($id);
		} catch (\Throwable) {
			return null;
		}
		return [$folder, (int)$folder->getStorage()->getCache()->getNumericStorageId()];
	}

	private function user(IUser $user, float $deadline, int $now): ?array {
		$home = $this->home($user->getUID());
		if ($home === null) {
			return null;
		}
		[$folder, $storage] = $home;
		$dir = $this->mimeTypes->getId('httpd/unix-directory');

		$used = 0;
		$files = 0;
		$folders = [];
		$biggest = [];
		$old = [];
		$oldFiles = 0;
		$oldBytes = 0;
		/** @var array<int, list<array>> $bySize */
		$bySize = [];

		$query = $this->db->getQueryBuilder();
		$query->select('fileid', 'path', 'size', 'mtime', 'encrypted', 'unencrypted_size')
			->from('filecache')
			->where($query->expr()->eq('storage', $query->createNamedParameter($storage)))
			->andWhere($query->expr()->like('path', $query->createNamedParameter('files/%')))
			->andWhere($query->expr()->neq('mimetype', $query->createNamedParameter($dir)));
		$result = $query->executeQuery();
		while ($row = $result->fetch()) {
			$size = (int)$row['size'];
			if ((int)$row['encrypted'] > 0 && (int)$row['unencrypted_size'] > 0) {
				$size = (int)$row['unencrypted_size'];
			}
			if ($size < 0) {
				continue;
			}
			$path = substr((string)$row['path'], strlen('files/'));
			$file = [
				'path' => $path,
				'size' => $size,
				'modified' => (int)$row['mtime'],
				'fileid' => (int)$row['fileid'],
			];
			$used += $size;
			$files++;
			$parts = explode('/', $path);
			if (count($parts) > 1) {
				self::add($folders, $parts[0], $size);
			}
			if (count($parts) > 2) {
				self::add($folders, $parts[0] . '/' . $parts[1], $size);
			}
			self::keep($biggest, $file);
			if ($file['modified'] > 0 && $file['modified'] < $now - self::OLD_AFTER) {
				$oldFiles++;
				$oldBytes += $size;
				self::keep($old, $file);
			}
			if ($size >= self::MIN_DUPLICATE) {
				$bySize[$size][] = $file;
			}
		}
		$result->closeCursor();

		// Fingerprint only the files some other file is as big as.
		$candidates = [];
		foreach ($bySize as $size => $same) {
			if (count($same) > 1) {
				foreach ($same as $file) {
					$candidates[] = $file;
				}
			}
		}
		unset($bySize);
		usort($candidates, fn ($a, $b) => $b['size'] <=> $a['size'] ?: $a['fileid'] <=> $b['fileid']);
		$hashes = $this->hasher->hashes($folder, $candidates, $deadline);
		$sets = [];
		foreach ($candidates as $file) {
			$hash = $hashes[$file['fileid']] ?? null;
			if ($hash !== null) {
				$sets[$file['size'] . ':' . $hash]['files'][] = $file;
				$sets[$file['size'] . ':' . $hash]['size'] = $file['size'];
				$sets[$file['size'] . ':' . $hash]['sha256'] = $hash;
			}
		}
		$duplicates = [];
		foreach ($sets as $set) {
			if (count($set['files']) < 2) {
				continue;
			}
			// The oldest copy is the one to keep.
			usort($set['files'], fn ($a, $b) => $a['modified'] <=> $b['modified'] ?: $a['fileid'] <=> $b['fileid']);
			$duplicates[] = ['size' => $set['size'], 'sha256' => $set['sha256'], 'files' => $set['files']];
		}
		usort($duplicates, fn ($a, $b) => $b['size'] * (count($b['files']) - 1) <=> $a['size'] * (count($a['files']) - 1));

		arsort($folders);
		$folderList = [];
		foreach (array_slice($folders, 0, self::MAX_FOLDERS, true) as $path => [$size, $count]) {
			$folderList[] = ['path' => (string)$path, 'size' => $size, 'files' => $count];
		}
		usort($folderList, fn ($a, $b) => $b['size'] <=> $a['size']);

		[$trash, $trashFiles] = $this->total($storage, 'files_trashbin/files/', $dir);
		[$versions, $versionsFiles] = $this->total($storage, 'files_versions/', $dir);

		return [
			'id' => $user->getUID(),
			'name' => $user->getDisplayName(),
			'email' => (string)($user->getEMailAddress() ?? ''),
			'quota' => (int)$user->getQuotaBytes(),
			'used' => $used,
			'files' => $files,
			'trash' => $trash,
			'trash_files' => $trashFiles,
			'versions' => $versions,
			'versions_files' => $versionsFiles,
			'folders' => $folderList,
			'biggest' => self::sorted($biggest),
			'old' => self::sorted($old),
			'old_files' => $oldFiles,
			'old_bytes' => $oldBytes,
			'duplicates' => array_slice($duplicates, 0, self::MAX_SETS),
			'hashed' => count(array_filter($candidates, fn ($f) => isset($hashes[$f['fileid']]))),
			'to_hash' => count($candidates),
		];
	}

	/** What the files under `$prefix` take, and how many there are. */
	private function total(int $storage, string $prefix, int $dir): array {
		$query = $this->db->getQueryBuilder();
		$query->select($query->func()->count('fileid', 'files'))
			->selectAlias($query->func()->sum('size'), 'bytes')
			->from('filecache')
			->where($query->expr()->eq('storage', $query->createNamedParameter($storage)))
			->andWhere($query->expr()->like('path', $query->createNamedParameter($prefix . '%')))
			->andWhere($query->expr()->neq('mimetype', $query->createNamedParameter($dir)))
			->andWhere($query->expr()->gte('size', $query->createNamedParameter(0)));
		$result = $query->executeQuery();
		$row = $result->fetch() ?: [];
		$result->closeCursor();
		return [(int)($row['bytes'] ?? 0), (int)($row['files'] ?? 0)];
	}

	private static function add(array &$folders, string $path, int $size): void {
		$folders[$path] ??= [0, 0];
		$folders[$path][0] += $size;
		$folders[$path][1]++;
	}

	/** Keeps the biggest `MAX_FILES`, trimming now and then. */
	private static function keep(array &$list, array $file): void {
		$list[] = $file;
		if (count($list) >= self::MAX_FILES * 2) {
			$list = self::sorted($list);
		}
	}

	private static function sorted(array $list): array {
		usort($list, fn ($a, $b) => $b['size'] <=> $a['size'] ?: $a['fileid'] <=> $b['fileid']);
		return array_slice($list, 0, self::MAX_FILES);
	}
}

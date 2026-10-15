<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Service;

use OCP\DB\QueryBuilder\IQueryBuilder;
use OCP\Files\Folder;
use OCP\IDBConnection;
use Psr\Log\LoggerInterface;

/**
 * SHA-256 fingerprints of whole files, read on the server, kept in the
 * `freesize_hashes` table until the file changes. Only these say two files
 * are the same: same size or name never does.
 */
class Hasher {
	private const TABLE = 'freesize_hashes';

	public function __construct(
		private IDBConnection $db,
		private LoggerInterface $logger,
	) {
	}

	/**
	 * Fingerprints of `$files` (in `$folder`, the user's files), from the
	 * table or read now while there's time.
	 *
	 * @param list<array{path: string, size: int, modified: int, fileid: int}> $files
	 * @return array<int, string> fileid => hex
	 */
	public function hashes(Folder $folder, array $files, float $deadline): array {
		$known = $this->known(array_column($files, 'fileid'));
		$hashes = [];
		$storage = $folder->getStorage();
		foreach ($files as $file) {
			$row = $known[$file['fileid']] ?? null;
			if ($row !== null && (int)$row['size'] === $file['size'] && (int)$row['mtime'] === $file['modified']) {
				$hashes[$file['fileid']] = (string)$row['sha256'];
				continue;
			}
			if (microtime(true) >= $deadline) {
				continue;
			}
			$hash = $this->read($storage, 'files/' . $file['path']);
			if ($hash === null) {
				continue;
			}
			$hashes[$file['fileid']] = $hash;
			$this->save($file, $hash, $row !== null);
		}
		return $hashes;
	}

	/** @return array<int, array> */
	private function known(array $ids): array {
		$known = [];
		foreach (array_chunk($ids, 1000) as $chunk) {
			$query = $this->db->getQueryBuilder();
			$query->select('fileid', 'size', 'mtime', 'sha256')
				->from(self::TABLE)
				->where($query->expr()->in('fileid', $query->createNamedParameter($chunk, IQueryBuilder::PARAM_INT_ARRAY)));
			$result = $query->executeQuery();
			while ($row = $result->fetch()) {
				$known[(int)$row['fileid']] = $row;
			}
			$result->closeCursor();
		}
		return $known;
	}

	private function read($storage, string $path): ?string {
		try {
			$stream = $storage->fopen($path, 'r');
			if ($stream === false) {
				return null;
			}
			$context = hash_init('sha256');
			hash_update_stream($context, $stream);
			fclose($stream);
			return hash_final($context);
		} catch (\Throwable $e) {
			$this->logger->info("FreeSize couldn't read $path: " . $e->getMessage(), ['app' => 'freesize']);
			return null;
		}
	}

	private function save(array $file, string $hash, bool $update): void {
		$query = $this->db->getQueryBuilder();
		if ($update) {
			$query->update(self::TABLE)
				->set('size', $query->createNamedParameter($file['size'], IQueryBuilder::PARAM_INT))
				->set('mtime', $query->createNamedParameter($file['modified'], IQueryBuilder::PARAM_INT))
				->set('sha256', $query->createNamedParameter($hash))
				->where($query->expr()->eq('fileid', $query->createNamedParameter($file['fileid'], IQueryBuilder::PARAM_INT)));
		} else {
			$query->insert(self::TABLE)->values([
				'fileid' => $query->createNamedParameter($file['fileid'], IQueryBuilder::PARAM_INT),
				'size' => $query->createNamedParameter($file['size'], IQueryBuilder::PARAM_INT),
				'mtime' => $query->createNamedParameter($file['modified'], IQueryBuilder::PARAM_INT),
				'sha256' => $query->createNamedParameter($hash),
			]);
		}
		$query->executeStatement();
	}

	/** Drops fingerprints of files that are gone. */
	public function prune(): void {
		$files = $this->db->getQueryBuilder();
		$files->select('fileid')->from('filecache');
		$query = $this->db->getQueryBuilder();
		$query->delete(self::TABLE)
			->where($query->expr()->notIn('fileid', $query->createFunction($files->getSQL())));
		$query->executeStatement();
	}
}

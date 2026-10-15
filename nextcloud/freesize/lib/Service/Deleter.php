<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Service;

use OCP\Files\Folder;
use OCP\Files\IRootFolder;
use OCP\Files\ISetupManager;
use OCP\Files\Node;
use OCP\Files\NotFoundException;
use OCP\IAppConfig;
use OCP\IDBConnection;
use OCP\IUser;
use OCP\IUserManager;
use OCP\IUserSession;
use OCP\Server;

/**
 * Deletes the Hub asks for, once allowed: through Nextcloud, so they go
 * into the user's trash bin and can be restored from "Deleted files".
 * Only the user's own files: never their whole folder, nothing shared with
 * them, nothing on external storage, and nothing at all without the trash
 * bin.
 */
class Deleter {
	public function __construct(
		private IRootFolder $root,
		private IUserManager $users,
		private IUserSession $session,
		private IDBConnection $db,
		private IAppConfig $appConfig,
	) {
	}

	/** Why the delete can't be done, or null. */
	public function check(array $action, array $covered, bool $trashBin): ?string {
		if (!$trashBin) {
			return "Nextcloud's trash bin is off, so a delete couldn't be undone: turn on the Deleted files app first";
		}
		$user = (string)($action['root'] ?? '');
		if (!in_array($user, $covered, true)) {
			return "the FreeSize app doesn't cover the Nextcloud user \"$user\"";
		}
		$paths = $action['paths'] ?? null;
		if (!is_array($paths) || $paths === []) {
			return 'the request names no files';
		}
		foreach ($paths as $path) {
			if (self::clean((string)$path) === null) {
				return "\"$path\" isn't a path inside the user's files";
			}
		}
		return null;
	}

	/** What the approver reads with the request. */
	public function note(array $action): string {
		$user = $this->users->get((string)($action['root'] ?? ''));
		$name = $user?->getDisplayName() ?? (string)($action['root'] ?? '');
		return "Moves the files to $name's trash bin in Nextcloud, where they can be restored from Deleted files until the trash bin is emptied.";
	}

	/** @return array{deleted: int, freed: int, failed: list<string>, detail: string} */
	public function delete(array $action, array $covered, bool $trashBin): array {
		$problem = $this->check($action, $covered, $trashBin);
		if ($problem !== null) {
			return ['deleted' => 0, 'freed' => 0, 'failed' => [$problem], 'detail' => $problem];
		}
		$id = (string)$action['root'];
		$user = $this->users->get($id);
		if ($user === null) {
			return ['deleted' => 0, 'freed' => 0, 'failed' => ["no user $id"], 'detail' => "no user $id"];
		}
		// The trash bin keeps what the user deletes: act as them, with
		// Nextcloud's files set up for them alone. The trash bin works out
		// whose trash a file goes to from that setup; set up for someone
		// else, it deletes the file for good.
		$before = $this->session->getUser();
		$this->session->setVolatileActiveUser($user);
		$deleted = 0;
		$freed = 0;
		$failed = [];
		try {
			$this->setUpFor($user);
			$folder = $this->root->getUserFolder($id);
			if (!$this->setUpAs($id)) {
				$why = "couldn't set up $id's files for the trash bin, so nothing was deleted";
				return ['deleted' => 0, 'freed' => 0, 'failed' => [$why], 'detail' => $why];
			}
			$limit = $this->trashLimit($id);
			foreach ($action['paths'] as $at => $path) {
				$path = (string)self::clean((string)$path);
				try {
					$node = $folder->get($path);
					$why = self::refuse($folder, $node);
					$size = (int)$node->getSize();
					if ($why === null && $limit >= 0 && $size >= $limit) {
						$why = 'it is bigger than the trash bin takes, so Nextcloud would delete it for good';
					}
					if ($why !== null) {
						$failed[] = "$path: $why";
						continue;
					}
					$started = time();
					$node->delete();
					if (!$this->inTrash($id, $node->getName(), $started)) {
						// Another app kept it out of the trash bin. Stop: the
						// rest might go the same way.
						$failed[] = "$path: Nextcloud deleted it without putting it in the trash bin";
						foreach (array_slice($action['paths'], $at + 1) as $rest) {
							$failed[] = "$rest: not done, after the one before didn't go to the trash bin";
						}
						break;
					}
					$deleted++;
					$freed += $size;
				} catch (NotFoundException) {
					$failed[] = "$path: not there any more";
				} catch (\Throwable $e) {
					$failed[] = "$path: " . $e->getMessage();
				}
			}
		} finally {
			$this->tearDown();
			$this->session->setVolatileActiveUser($before);
		}
		$detail = $deleted === 0
			? 'nothing was moved to the trash bin'
			: sprintf('moved %d item%s to %s\'s trash bin in Nextcloud', $deleted, $deleted === 1 ? '' : 's', $user->getDisplayName());
		return ['deleted' => $deleted, 'freed' => $freed, 'failed' => $failed, 'detail' => $detail];
	}

	private function setUpFor(IUser $user): void {
		if (interface_exists(ISetupManager::class)) {
			$setup = Server::get(ISetupManager::class);
			$setup->tearDown();
			$setup->setupForUser($user);
		} else {
			// Before Nextcloud 34.
			\OC_Util::tearDownFS();
			\OC_Util::setupFS($user->getUID());
		}
	}

	private function tearDown(): void {
		if (interface_exists(ISetupManager::class)) {
			Server::get(ISetupManager::class)->tearDown();
		} else {
			\OC_Util::tearDownFS();
		}
	}

	/** Nextcloud's files are set up for `$id`: what the trash bin goes by. */
	private function setUpAs(string $id): bool {
		return class_exists(\OC\Files\Filesystem::class)
			&& rtrim((string)\OC\Files\Filesystem::getRoot(), '/') === "/$id/files";
	}

	/** The most the trash bin takes of one file for `$id`; -1 for no limit. */
	private function trashLimit(string $id): int {
		$query = $this->db->getQueryBuilder();
		$query->select('configvalue')
			->from('preferences')
			->where($query->expr()->eq('userid', $query->createNamedParameter($id)))
			->andWhere($query->expr()->eq('appid', $query->createNamedParameter('files_trashbin')))
			->andWhere($query->expr()->eq('configkey', $query->createNamedParameter('trashbin_size')));
		$result = $query->executeQuery();
		$own = $result->fetchOne();
		$result->closeCursor();
		if (is_numeric($own) && (int)$own > -1) {
			return (int)$own;
		}
		$all = $this->appConfig->getValueString('files_trashbin', 'trashbin_size', '-1');
		return is_numeric($all) ? (int)$all : -1;
	}

	/** The trash bin took a file named `$name` from `$id` since `$since`. */
	private function inTrash(string $id, string $name, int $since): bool {
		$query = $this->db->getQueryBuilder();
		$query->select('id')
			->from('files_trash')
			->where($query->expr()->eq('user', $query->createNamedParameter($id)))
			->andWhere($query->expr()->eq('id', $query->createNamedParameter($name)))
			->andWhere($query->expr()->gte('timestamp', $query->createNamedParameter((string)($since - 1))))
			->setMaxResults(1);
		$result = $query->executeQuery();
		$found = $result->fetchOne() !== false;
		$result->closeCursor();
		return $found;
	}

	private static function refuse(Folder $folder, Node $node): ?string {
		if ($node->getId() === $folder->getId()) {
			return 'that is the whole of their files';
		}
		if ($node->getMountPoint()->getMountPoint() !== $folder->getMountPoint()->getMountPoint()) {
			return 'it is shared with them or on external storage, not their own file';
		}
		if (!str_starts_with($node->getInternalPath(), 'files/')) {
			return 'it is outside their files';
		}
		if (!$node->isDeletable()) {
			return 'Nextcloud doesn\'t let them delete it';
		}
		return null;
	}

	/** `a/b.txt` from the Hub's path, or null if it leaves the user's files. */
	public static function clean(string $path): ?string {
		if (str_contains($path, "\0") || str_contains($path, '\\')) {
			return null;
		}
		$parts = array_values(array_filter(explode('/', $path), fn ($p) => $p !== '' && $p !== '.'));
		if ($parts === [] || in_array('..', $parts, true)) {
			return null;
		}
		return implode('/', $parts);
	}
}

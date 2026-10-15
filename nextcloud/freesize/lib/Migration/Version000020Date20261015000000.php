<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Migration;

use Closure;
use OCP\DB\ISchemaWrapper;
use OCP\DB\Types;
use OCP\Migration\IOutput;
use OCP\Migration\SimpleMigrationStep;

/** The table of file fingerprints (0.20). */
class Version000020Date20261015000000 extends SimpleMigrationStep {
	public function changeSchema(IOutput $output, Closure $schemaClosure, array $options): ?ISchemaWrapper {
		/** @var ISchemaWrapper $schema */
		$schema = $schemaClosure();
		if ($schema->hasTable('freesize_hashes')) {
			return null;
		}
		$table = $schema->createTable('freesize_hashes');
		$table->addColumn('fileid', Types::BIGINT, ['notnull' => true, 'unsigned' => true]);
		$table->addColumn('size', Types::BIGINT, ['notnull' => true]);
		$table->addColumn('mtime', Types::BIGINT, ['notnull' => true]);
		$table->addColumn('sha256', Types::STRING, ['notnull' => true, 'length' => 64]);
		$table->setPrimaryKey(['fileid']);
		$table->addIndex(['sha256'], 'freesize_hashes_sha');
		return $schema;
	}
}

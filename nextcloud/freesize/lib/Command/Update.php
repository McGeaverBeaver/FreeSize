<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Command;

use OCA\FreeSize\AppInfo\Application;
use OCA\FreeSize\Service\Updater;
use Symfony\Component\Console\Command\Command;
use Symfony\Component\Console\Input\InputInterface;
use Symfony\Component\Console\Input\InputOption;
use Symfony\Component\Console\Output\OutputInterface;

class Update extends Command {
	public function __construct(
		private Updater $updater,
	) {
		parent::__construct();
	}

	protected function configure(): void {
		$this->setName('freesize:update')
			->setDescription('Update the app to the newest FreeSize release (signed by the release key), or from a tarball')
			->addOption('check', null, InputOption::VALUE_NONE, 'Only say whether a newer release is out')
			->addOption('file', null, InputOption::VALUE_REQUIRED, 'Install this freesize-nextcloud-<version>.tar.gz instead of downloading one');
	}

	protected function execute(InputInterface $input, OutputInterface $output): int {
		try {
			if ($input->getOption('check')) {
				$found = $this->updater->check();
				$output->writeln($found === null
					? 'This is the newest version (' . Application::VERSION . ').'
					: "FreeSize {$found['version']} is out: {$found['url']}. Install it with occ freesize:update.");
				return 0;
			}
			$file = $input->getOption('file');
			$output->writeln($this->updater->update(is_string($file) && $file !== '' ? $file : null));
			return 0;
		} catch (\RuntimeException $e) {
			$output->writeln('<error>' . $e->getMessage() . '</error>');
			return 1;
		}
	}
}

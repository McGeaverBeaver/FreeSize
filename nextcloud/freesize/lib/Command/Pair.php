<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Command;

use OCA\FreeSize\Hub\HubException;
use OCA\FreeSize\Service\Agent;
use Symfony\Component\Console\Command\Command;
use Symfony\Component\Console\Input\InputArgument;
use Symfony\Component\Console\Input\InputInterface;
use Symfony\Component\Console\Input\InputOption;
use Symfony\Component\Console\Output\OutputInterface;

class Pair extends Command {
	public function __construct(
		private Agent $agent,
	) {
		parent::__construct();
	}

	protected function configure(): void {
		$this->setName('freesize:pair')
			->setDescription('Pair this Nextcloud with a FreeSize Hub')
			->addArgument('link', InputArgument::REQUIRED, 'The link from the Hub\'s "Pair a device", or the Hub\'s address')
			->addArgument('code', InputArgument::OPTIONAL, 'The pairing code, when the first argument is the Hub\'s address', '')
			->addOption('fingerprint', null, InputOption::VALUE_REQUIRED, 'The Hub\'s fingerprint as the dashboard shows it, when the link has none');
	}

	protected function execute(InputInterface $input, OutputInterface $output): int {
		try {
			$name = $this->agent->pair($input->getArgument('link'), (string)$input->getArgument('code'), $input->getOption('fingerprint'));
		} catch (HubException $e) {
			$output->writeln('<error>' . $e->getMessage() . '</error>');
			return 1;
		}
		$output->writeln("Paired with $name.");
		$output->writeln('Next: choose the approver (`occ freesize:approver FINGERPRINT`) and the users it covers (`occ freesize:users --add USER`).');
		return 0;
	}
}

<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Command;

use OCA\FreeSize\Service\Agent;
use Symfony\Component\Console\Command\Command;
use Symfony\Component\Console\Input\InputInterface;
use Symfony\Component\Console\Output\OutputInterface;

class RunNow extends Command {
	public function __construct(
		private Agent $agent,
	) {
		parent::__construct();
	}

	protected function configure(): void {
		$this->setName('freesize:run')
			->setDescription('Report to the Hub and take its requests now, as the background job does every five minutes');
	}

	protected function execute(InputInterface $input, OutputInterface $output): int {
		$said = $this->agent->run(true);
		$output->writeln($said === '' ? 'Nothing new.' : $said);
		return $this->agent->state()->status()['last_error'] === '' ? 0 : 1;
	}
}

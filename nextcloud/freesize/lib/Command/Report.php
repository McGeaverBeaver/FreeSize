<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Command;

use OCA\FreeSize\Service\Agent;
use OCA\FreeSize\Service\Scanner;
use Symfony\Component\Console\Command\Command;
use Symfony\Component\Console\Input\InputInterface;
use Symfony\Component\Console\Output\OutputInterface;

/** Shows exactly what goes to the Hub, without sending it. */
class Report extends Command {
	public function __construct(
		private Agent $agent,
		private Scanner $scanner,
	) {
		parent::__construct();
	}

	protected function configure(): void {
		$this->setName('freesize:report')
			->setDescription('Print the report FreeSize sends the Hub, as JSON, without sending it');
	}

	protected function execute(InputInterface $input, OutputInterface $output): int {
		$report = $this->scanner->report($this->agent->state()->users(), $this->agent->trashBin(), 120, time());
		$output->writeln(json_encode(['info' => $this->agent->info(), 'cloud' => $report], JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE));
		return 0;
	}
}

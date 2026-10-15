<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Command;

use OCA\FreeSize\Service\Agent;
use Symfony\Component\Console\Command\Command;
use Symfony\Component\Console\Input\InputInterface;
use Symfony\Component\Console\Output\OutputInterface;

class Unpair extends Command {
	public function __construct(
		private Agent $agent,
	) {
		parent::__construct();
	}

	protected function configure(): void {
		$this->setName('freesize:unpair')
			->setDescription('Forget the FreeSize Hub; remove this device from the dashboard too');
	}

	protected function execute(InputInterface $input, OutputInterface $output): int {
		$this->agent->unpair();
		$output->writeln('Forgot the Hub. Remove "' . $this->agent->state()->name() . '" from the dashboard too, so its reports go away.');
		return 0;
	}
}

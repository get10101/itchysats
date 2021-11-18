import { ButtonGroup, ButtonGroupProps, IconButton } from "@chakra-ui/react";
import * as React from "react";
import { FaGithub, FaTelegram, FaTwitter } from "react-icons/fa";

export const SocialLinks = (props: ButtonGroupProps) => (
    <ButtonGroup variant="ghost" color="gray.600" {...props}>
        <IconButton
            as="a"
            href="https://t.me/joinchat/ULycH50PLV1jOTI0"
            aria-label="Telegram join link"
            icon={<FaTelegram fontSize="20px" />}
        />
        <IconButton
            as="a"
            href="https://twitter.com/itchysats"
            aria-label="Twitter"
            icon={<FaTwitter fontSize="20px" />}
        />
        <IconButton
            as="a"
            href="https://github.com/itchysats/itchysats"
            aria-label="GitHub"
            icon={<FaGithub fontSize="20px" />}
        />
    </ButtonGroup>
);

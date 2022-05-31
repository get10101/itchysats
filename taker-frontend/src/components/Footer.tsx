import { ExternalLinkIcon } from "@chakra-ui/icons";
import {
    Box,
    Button,
    Center,
    Divider,
    HStack,
    IconButton,
    Link,
    Modal,
    ModalBody,
    ModalCloseButton,
    ModalContent,
    ModalFooter,
    ModalHeader,
    ModalOverlay,
    Text,
    useColorModeValue,
    useDisclosure,
} from "@chakra-ui/react";
import { FeedbackFish } from "@feedback-fish/react";
import * as React from "react";
import { FaInfo, FaRegCommentDots } from "react-icons/all";
import { FAQ_URL, FOOTER_HEIGHT } from "../App";
import { SocialLinks } from "./SocialLinks";

function TextDivider() {
    return <Divider orientation={"vertical"} borderColor={useColorModeValue("black", "white")} height={"20px"} />;
}

interface FooterProps {
    taker_id: string;
    peer_id: string;
}

export default function Footer({ taker_id, peer_id }: FooterProps) {
    const { isOpen, onOpen, onClose } = useDisclosure();

    return (
        <Box
            bg={useColorModeValue("gray.100", "gray.900")}
            color={useColorModeValue("gray.700", "gray.200")}
        >
            <Center>
                <HStack h={`${FOOTER_HEIGHT}px`} alignItems={"center"}>
                    <Link
                        href={FAQ_URL}
                        isExternal
                    >
                        <HStack>
                            <Text fontSize={"20"} fontWeight={"bold"}>FAQ</Text>
                            <ExternalLinkIcon boxSize={5} />
                        </HStack>
                    </Link>
                    <TextDivider />
                    <Text fontSize={"20"} fontWeight={"bold"} display={["none", "none", "inherit"]}>Contact us:</Text>
                    <SocialLinks />
                    <TextDivider />
                    <IconButton
                        aria-label={"itchysats-about"}
                        icon={<FaInfo />}
                        fontSize="20px"
                        color={"gray.600"}
                        onClick={onOpen}
                    />
                    <Modal isOpen={isOpen} onClose={onClose}>
                        <ModalOverlay />
                        <ModalContent>
                            <ModalHeader>ItchySats Info</ModalHeader>
                            <ModalCloseButton />
                            <ModalBody>
                                <Text>Your Taker ID: {taker_id}</Text>
                                <Text>Your Peer ID: {peer_id}</Text>
                            </ModalBody>
                            <ModalFooter>
                                <Button colorScheme="blue" mr={3} onClick={onClose}>
                                    Close
                                </Button>
                            </ModalFooter>
                        </ModalContent>
                    </Modal>
                    <TextDivider />
                    <FeedbackFish
                        projectId="c1260a96cdb3d8"
                        metadata={{ position: "footer", customerid: taker_id, peerid: peer_id }}
                    >
                        <Button
                            fontSize={"20"}
                            color={useColorModeValue("black", "white")}
                            leftIcon={<FaRegCommentDots />}
                            variant={"ghost"}
                        >
                            <Text display={["none", "none", "inherit"]}>Send Feedback</Text>
                        </Button>
                    </FeedbackFish>
                </HStack>
            </Center>
        </Box>
    );
}

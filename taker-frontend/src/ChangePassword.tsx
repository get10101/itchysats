import {
    Alert,
    AlertDescription,
    AlertIcon,
    Button,
    Flex,
    Heading,
    Input,
    InputGroup,
    InputRightElement,
    Text,
    VStack,
} from "@chakra-ui/react";
import * as React from "react";
import { FormEvent } from "react";
import { useState } from "react";
import useAuth from "./authentication/useAuth";

export default function ChangePassword() {
    const { changePassword, loading, error } = useAuth();
    const [showPw1, setShowPw1] = useState(false);
    const [showPw2, setShowPw2] = useState(false);
    const handlePw1Click = () => setShowPw1(!showPw1);
    const handlePw2Click = () => setShowPw2(!showPw2);
    const [password, setPassword] = useState("");
    const [confirmedPassword, setConfirmedPassword] = useState("");
    const [errorMessage, setErrorMessage] = useState("");

    function handleSubmit(event: FormEvent<HTMLFormElement>) {
        event.preventDefault();
        if (confirmedPassword !== password) {
            setErrorMessage("Passwords are not equal");
            return;
        }
        changePassword(password);
    }

    return (
        <form onSubmit={handleSubmit}>
            <Flex justifyContent={"center"} alignItems={"center"} height={"100vh"}>
                <VStack spacing="6">
                    <Heading>Please change your password</Heading>
                    <Text as={"i"} align={"center"}>
                        Remember to store this password securely.<br />
                        It is your only way into ItchySats!
                    </Text>
                    <VStack>
                        <InputGroup size="md">
                            <Input
                                pr="4.5rem"
                                type={showPw1 ? "text" : "password"}
                                placeholder="Enter password"
                                value={password}
                                onChange={(e) => setPassword(e.target.value)}
                            />
                            <InputRightElement width="4.5rem">
                                <Button h="1.75rem" size="sm" onClick={handlePw1Click}>
                                    {showPw1 ? "Hide" : "Show"}
                                </Button>
                            </InputRightElement>
                        </InputGroup>
                        <InputGroup size="md">
                            <Input
                                pr="4.5rem"
                                type={showPw2 ? "text" : "password"}
                                placeholder="Confirm password"
                                value={confirmedPassword}
                                onChange={(e) => setConfirmedPassword(e.target.value)}
                            />
                            <InputRightElement width="4.5rem">
                                <Button h="1.75rem" size="sm" onClick={handlePw2Click}>
                                    {showPw2 ? "Hide" : "Show"}
                                </Button>
                            </InputRightElement>
                        </InputGroup>
                    </VStack>

                    <Button disabled={loading} variant={"solid"} colorScheme={"blue"} isLoading={loading} type="submit">
                        Submit
                    </Button>

                    {error
                        && (
                            <Alert status="error">
                                <AlertIcon />
                                <AlertDescription>{error.detail}</AlertDescription>
                            </Alert>
                        )}
                    {errorMessage
                        && (
                            <Alert status="error">
                                <AlertIcon />
                                <AlertDescription>{errorMessage}</AlertDescription>
                            </Alert>
                        )}
                </VStack>
            </Flex>
        </form>
    );
}
